//! Explicit Gemini Developer API cachedContents lifecycle.
//! Wire contract: https://ai.google.dev/api/caching. Not Vertex or a gateway.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::{AiError, ConfigError, HookModelContext, Model, PayloadHook, Protocol, Request};

/// Hard-bounded, opt-in limits for one native cache session.
#[derive(Clone, Copy, Debug)]
pub struct GoogleCacheLimits {
    /// Simultaneously owned resources, including ambiguous creates (1..=8).
    pub max_entries: usize,
    /// Maximum requested TTL, in whole seconds (1..=3600 seconds).
    pub max_ttl: Duration,
    /// Total create/refresh/delete attempts, reserving deletes (2..=64).
    pub max_operations: usize,
    /// Whole-operation deadline, including auth and body (1..=30 seconds).
    pub operation_timeout: Duration,
}

impl Default for GoogleCacheLimits {
    fn default() -> Self {
        Self {
            max_entries: 4,
            max_ttl: Duration::from_secs(300),
            max_operations: 32,
            operation_timeout: Duration::from_secs(10),
        }
    }
}

/// An explicit, non-cloneable owner of a bounded set of Gemini cache resources.
///
/// Construction does no credential resolution or network I/O. Call `close()`
/// at session end, including after errors. Drop never spawns network work;
/// finite provider TTLs bound retention after cancellation or process death.
pub struct GoogleCacheSession {
    model: Model,
    http: reqwest::Client,
    limits: GoogleCacheLimits,
    entries: Vec<Entry>,
    remaining: usize,
    closed: bool,
    closing: bool,
}

struct Entry {
    // None reserves capacity after an ambiguous or cancelled create. It cannot
    // be evicted: no name was returned with which to prove remote deletion.
    name: Option<String>,
    active: Arc<std::sync::atomic::AtomicBool>,
}

/// An owned cache prefix, usable as a native `HostRequestOptions.on_payload` hook.
///
/// The hook checks model/provider/API identity and prefix values before removing the
/// cached prefix from the outgoing request. Expired/deleted/closed handles or
/// changed prefixes leave the ordinary full request intact, keeping turns alive.
#[derive(Clone)]
pub struct GoogleCachedContent {
    name: String,
    model: HookModelContext,
    prefix: Value,
    deadline: Arc<std::sync::Mutex<Instant>>,
    active: Arc<std::sync::atomic::AtomicBool>,
}

impl GoogleCachedContent {
    /// Provider resource name; not a URL or a durable cross-session cursor.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl PayloadHook for GoogleCachedContent {
    fn on_payload(
        &self,
        mut payload: Value,
        model: &HookModelContext,
    ) -> Result<Option<Value>, AiError> {
        if model != &self.model
            || !self.active.load(std::sync::atomic::Ordering::Acquire)
            || Instant::now() >= *self.deadline.lock().expect("cache deadline")
        {
            return Ok(None);
        }
        let Some(contents) = payload.get("contents").and_then(Value::as_array) else {
            return Ok(None);
        };
        let cached = self.prefix["contents"].as_array().expect("encoded prefix");
        if contents.len() < cached.len()
            || contents[..cached.len() - 1] != cached[..cached.len() - 1]
            || ["systemInstruction", "tools", "toolConfig"]
                .iter()
                .any(|key| payload.get(key) != self.prefix.get(key))
        {
            return Ok(None);
        }
        let last = cached.len() - 1;
        let old_parts = cached[last]["parts"].as_array().expect("encoded parts");
        let Some(new_parts) = contents[last].get("parts").and_then(Value::as_array) else {
            return Ok(None);
        };
        if contents[last]["role"] != cached[last]["role"]
            || !new_parts.starts_with(old_parts)
            || contents[last]
                .as_object()
                .is_none_or(|object| object.len() != 2)
        {
            return Ok(None);
        }
        let partial = new_parts.len() > old_parts.len();
        if !partial && contents.len() == cached.len() {
            return Ok(None);
        }
        let root = payload.as_object_mut().expect("contents object");
        let contents = root.get_mut("contents").unwrap().as_array_mut().unwrap();
        if partial {
            contents.drain(..last);
            contents[0]["parts"]
                .as_array_mut()
                .unwrap()
                .drain(..old_parts.len());
        } else {
            contents.drain(..cached.len());
        }
        for key in ["systemInstruction", "tools", "toolConfig"] {
            root.remove(key);
        }
        root.insert("cachedContent".into(), json!(self.name));
        Ok(Some(payload))
    }
}

fn failure(stage: &'static str) -> AiError {
    ConfigError::Parse(format!(
        "Google cache {stage}; continue without explicit caching"
    ))
    .into()
}

impl GoogleCacheSession {
    /// Creates a local owner for the direct Gemini Developer API route only.
    /// No network calls, credential reads, probes, timers or background tasks.
    pub fn new(model: Model, limits: GoogleCacheLimits) -> Result<Self, AiError> {
        let base = &model.endpoint.base_url;
        if model.spec.protocol != Protocol::GoogleGenerativeAi
            || model.endpoint.id.0 != "gemini"
            || base.scheme() != "https"
            || base.host_str() != Some("generativelanguage.googleapis.com")
            || base.port().is_some()
            || base.path() != "/v1beta/"
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(failure("requires a direct Gemini Developer API route"));
        }
        crate::catalog::validate_model_spec(&model.spec)?;
        Self::local(model, limits)
    }

    fn local(model: Model, limits: GoogleCacheLimits) -> Result<Self, AiError> {
        if !(1..=8).contains(&limits.max_entries)
            || !(2..=64).contains(&limits.max_operations)
            || limits.max_operations < limits.max_entries * 2
            || limits.max_ttl.is_zero()
            || limits.max_ttl > Duration::from_secs(3600)
            || limits.max_ttl.subsec_nanos() != 0
            || limits.operation_timeout < Duration::from_secs(1)
            || limits.operation_timeout > Duration::from_secs(30)
        {
            return Err(failure("limits are invalid"));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| failure("transport initialization failed"))?;
        Ok(Self {
            model,
            http,
            limits,
            entries: vec![],
            remaining: limits.max_operations,
            closed: false,
            closing: false,
        })
    }

    fn ttl(&self, ttl: Duration) -> Result<String, AiError> {
        if ttl.is_zero() || ttl > self.limits.max_ttl || ttl.subsec_nanos() != 0 {
            return Err(failure("TTL exceeds the session limit"));
        }
        Ok(format!("{}s", ttl.as_secs()))
    }

    fn admit(&mut self, reserve: usize) -> Result<(), AiError> {
        if self.closed || self.remaining <= reserve {
            return Err(failure("operation limit reached or session closed"));
        }
        self.remaining -= 1;
        Ok(())
    }

    /// Creates one resource from an explicitly selected canonical prefix.
    /// At most 2 MiB of encoded prefix is admitted. No retries or implicit use.
    /// Ambiguous/cancelled creates retain a capacity slot until session end.
    pub async fn create(
        &mut self,
        prefix: &Request,
        ttl: Duration,
    ) -> Result<GoogleCachedContent, AiError> {
        let ttl_wire = self.ttl(ttl)?;
        if !matches!(prefix.messages.last(), Some(crate::Message::User(_))) {
            return Err(failure("prefix must end with a user turn"));
        }
        if self.closing {
            return Err(failure("session is closing"));
        }
        if self.entries.len() >= self.limits.max_entries {
            return Err(failure("entry limit reached"));
        }
        let encoded = super::build_request(&self.model, prefix)?;
        if encoded.body.len() > 2 * 1024 * 1024 {
            return Err(failure("prefix exceeds 2 MiB"));
        }
        let mut prefix: Value = serde_json::from_slice(&encoded.body).expect("codec JSON");
        prefix.as_object_mut().unwrap().remove("generationConfig");
        if prefix["contents"].as_array().is_none_or(Vec::is_empty) {
            return Err(failure("prefix has no cacheable content"));
        }
        let mut body = prefix.clone();
        body["model"] = json!(format!("models/{}", self.model.spec.api_name));
        body["ttl"] = json!(ttl_wire);
        if serde_json::to_vec(&body).expect("codec JSON").len() > 2 * 1024 * 1024 {
            return Err(failure("create body exceeds 2 MiB"));
        }
        self.admit(self.entries.len() + 1)?; // reserve cleanup for the new resource
        let active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.entries.push(Entry {
            name: None,
            active: active.clone(),
        });
        let index = self.entries.len() - 1;
        let value = self
            .send(http::Method::POST, "cachedContents", Some(body))
            .await?;
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| valid_name(name))
            .ok_or_else(|| failure("create returned an invalid resource name"))?
            .to_owned();
        if self
            .entries
            .iter()
            .any(|e| e.name.as_deref() == Some(&name))
        {
            return Err(failure("create returned a duplicate resource"));
        }
        self.entries[index].name = Some(name.clone());
        active.store(true, std::sync::atomic::Ordering::Release);
        Ok(GoogleCachedContent {
            name,
            model: HookModelContext::from_model(&self.model),
            prefix,
            // Start locally after response only for bookkeeping, but expire the
            // handle conservatively from dispatch (subtract the whole deadline).
            deadline: Arc::new(std::sync::Mutex::new(
                Instant::now() + ttl.saturating_sub(self.limits.operation_timeout),
            )),
            active,
        })
    }

    fn owned(&self, cache: &GoogleCachedContent) -> Result<usize, AiError> {
        self.entries
            .iter()
            .position(|entry| {
                entry.name.as_deref() == Some(cache.name())
                    && Arc::ptr_eq(&entry.active, &cache.active)
            })
            .ok_or_else(|| failure("resource is not owned by this session"))
    }

    /// Refreshes only the TTL of an owned resource, without uploading its prefix.
    /// Failed refreshes disable the local handle; generation can use full context.
    pub async fn refresh(
        &mut self,
        cache: &GoogleCachedContent,
        ttl: Duration,
    ) -> Result<(), AiError> {
        let ttl_wire = self.ttl(ttl)?;
        if self.closing {
            return Err(failure("session is closing"));
        }
        self.owned(cache)?;
        self.admit(self.entries.len())?;
        cache
            .active
            .store(false, std::sync::atomic::Ordering::Release);
        self.send(
            http::Method::PATCH,
            &format!("{}?updateMask=ttl", cache.name()),
            Some(json!({"ttl":ttl_wire})),
        )
        .await?;
        *cache.deadline.lock().expect("cache deadline") =
            Instant::now() + ttl.saturating_sub(self.limits.operation_timeout);
        cache
            .active
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Deletes one owned resource. A provider 404 is already deleted.
    pub async fn delete(&mut self, cache: &GoogleCachedContent) -> Result<(), AiError> {
        let index = self.owned(cache)?;
        if self.closing {
            return Err(failure("session is closing"));
        }
        self.delete_index(index, false).await
    }

    async fn delete_index(&mut self, index: usize, cleanup: bool) -> Result<(), AiError> {
        self.admit(if cleanup { 0 } else { self.entries.len() })?;
        let entry = &self.entries[index];
        entry
            .active
            .store(false, std::sync::atomic::Ordering::Release);
        let name = entry.name.as_deref().expect("named entry");
        self.send(http::Method::DELETE, name, None).await?;
        self.entries.remove(index);
        Ok(())
    }

    /// Ends this session and attempts every known resource's deletion once.
    /// Cleanup is bounded by `max_entries * operation_timeout`. All handles are
    /// disabled, even when deletion fails. Unknown creates expire by provider TTL.
    /// Repeated close calls do no I/O; errors never authorize inference retries.
    pub async fn close(&mut self) -> Result<(), AiError> {
        if self.closed {
            return Ok(());
        }
        self.closing = true;
        for entry in &self.entries {
            entry
                .active
                .store(false, std::sync::atomic::Ordering::Release);
        }
        let mut error = self
            .entries
            .iter()
            .any(|entry| entry.name.is_none())
            .then(|| failure("cleanup has an unknown resource; provider TTL bounds retention"));
        for index in (0..self.entries.len()).rev() {
            if self.entries[index].name.is_some() {
                if let Err(failure) = self.delete_index(index, true).await {
                    if error.is_none() {
                        error = Some(failure);
                    }
                }
            }
        }
        self.closed = true;
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }

    async fn send(
        &self,
        method: http::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, AiError> {
        tokio::time::timeout(self.limits.operation_timeout, async {
            let url = self
                .model
                .endpoint
                .base_url
                .join(path)
                .expect("validated cache path");
            let auth = crate::auth::resolve_headers(&self.model.endpoint.auth).await?;
            let mut headers = self.model.endpoint.default_headers.clone();
            for (name, value) in &auth.headers {
                headers.insert(name.clone(), value.clone());
            }
            let mut request = self.http.request(method.clone(), url).headers(headers);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let mut response = request
                .send()
                .await
                .map_err(|_| failure("request failed"))?;
            let status = response.status();
            if method == http::Method::DELETE && status == http::StatusCode::NOT_FOUND {
                return Ok(json!({}));
            }
            if !status.is_success() {
                return Err(failure("service rejected the operation"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| failure("response failed"))?
            {
                if bytes.len().saturating_add(chunk.len()) > 16 * 1024 {
                    return Err(failure("response exceeds 16 KiB"));
                }
                bytes.extend_from_slice(&chunk);
            }
            if method == http::Method::DELETE {
                return Ok(json!({}));
            }
            serde_json::from_slice(&bytes).map_err(|_| failure("response is invalid JSON"))
        })
        .await
        .map_err(|_| failure("operation deadline exceeded"))?
    }
}

impl Drop for GoogleCacheSession {
    fn drop(&mut self) {
        for entry in &self.entries {
            entry
                .active
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

fn valid_name(name: &str) -> bool {
    name.strip_prefix("cachedContents/").is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

#[cfg(test)]
mod tests;

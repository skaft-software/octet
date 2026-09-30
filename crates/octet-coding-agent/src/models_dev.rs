//! Live models.dev metadata: display names, pricing and capability records.
//!
//! An interactive session refreshes `https://models.dev/api.json` in the
//! background at most every [`REFRESH_INTERVAL`], revalidating with the ETag.
//! Records are checked against the compiled snapshot by
//! [`octet_ai::model_metadata::live_metadata_from_models_dev`]; the accepted
//! ones are cached at `~/.octet/cache/models-dev/metadata.json` and installed
//! for every later lookup, and each launch installs the cache before its first
//! model catalog is built. `--offline` skips the refresh. A cache written by
//! another octet version is ignored, because its records were checked against
//! a different compiled snapshot. Nothing here ever blocks startup: a failed
//! refresh keeps the metadata already in use.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use octet_ai::model_metadata::LiveModelMetadata;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const API_URL: &str = "https://models.dev/api.json";
/// How long fetched metadata stays fresh before the next background refresh.
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ETAG_BYTES: usize = 256;
const CACHE_SCHEMA: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    schema: u32,
    octet_version: String,
    /// Last successful fetch or ETag revalidation, in Unix milliseconds.
    fetched_at_ms: u64,
    etag: Option<String>,
    source_sha256: String,
    source_bytes: u64,
    /// `key: reason` for each record that kept the built-in data.
    rejected: Vec<String>,
    metadata: LiveModelMetadata,
}

/// `~/.octet/cache/models-dev/metadata.json`, when a home directory exists.
pub(crate) fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        home.join(".octet")
            .join("cache")
            .join("models-dev")
            .join("metadata.json")
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn read_cache(path: &Path) -> Option<CacheFile> {
    let bytes = octet_agent::secure_fs::read_private_file_bounded(path, MAX_CACHE_BYTES).ok()?;
    let cache: CacheFile = serde_json::from_slice(&bytes).ok()?;
    (cache.schema == CACHE_SCHEMA && cache.octet_version == env!("CARGO_PKG_VERSION"))
        .then_some(cache)
}

fn write_cache(path: &Path, cache: &CacheFile) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(cache)?;
    octet_agent::secure_fs::write_private_atomic(path, &bytes, MAX_CACHE_BYTES)?;
    Ok(())
}

/// Install the cached live metadata once per process, before the first model
/// catalog is built. Unit tests never read the developer's home directory.
pub(crate) fn install_cached() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if cfg!(test) {
        return;
    }
    ONCE.call_once(|| {
        if let Some(cache) = cache_path().as_deref().and_then(read_cache) {
            octet_ai::model_metadata::install_live_metadata(cache.metadata);
        }
    });
}

/// A missing, stale or future-dated cache needs a refresh.
fn needs_refresh(fetched_at_ms: Option<u64>, now_ms: u64) -> bool {
    let interval = u64::try_from(REFRESH_INTERVAL.as_millis()).unwrap_or(u64::MAX);
    fetched_at_ms.is_none_or(|fetched| fetched > now_ms || now_ms - fetched >= interval)
}

#[derive(Debug)]
enum RefreshOutcome {
    Fresh,
    NotModified,
    Updated(LiveModelMetadata),
}

/// Refresh in the background when the cache is missing or stale. Failures are
/// silent: the metadata already installed stays in use.
pub(crate) async fn refresh(offline: bool) {
    if offline || cfg!(test) {
        return;
    }
    let Some(path) = cache_path() else {
        return;
    };
    let Ok(client) = fetch_client() else {
        return;
    };
    if let Ok(RefreshOutcome::Updated(metadata)) =
        refresh_at(&client, API_URL, &path, now_ms()).await
    {
        octet_ai::model_metadata::install_live_metadata(metadata);
    }
}

/// No redirects, retries or credentials, and a bounded deadline. Like the
/// startup update check, optional background traffic never picks up
/// environment proxy credentials.
fn fetch_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .user_agent(concat!("octet/", env!("CARGO_PKG_VERSION")))
        .build()
}

async fn refresh_at(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    now_ms: u64,
) -> anyhow::Result<RefreshOutcome> {
    let cached = read_cache(path);
    if !needs_refresh(cached.as_ref().map(|cache| cache.fetched_at_ms), now_ms) {
        return Ok(RefreshOutcome::Fresh);
    }
    let mut request = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(etag) = cached.as_ref().and_then(|cache| cache.etag.as_deref()) {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let response = request.send().await?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        let mut cache = cached.ok_or_else(|| anyhow::anyhow!("304 without a cached catalog"))?;
        cache.fetched_at_ms = now_ms;
        write_cache(path, &cache)?;
        return Ok(RefreshOutcome::NotModified);
    }
    let response = response.error_for_status()?;
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= MAX_ETAG_BYTES)
        .map(str::to_owned);
    let body = read_bounded(response).await?;
    let catalog: serde_json::Value = serde_json::from_slice(&body)?;
    let (metadata, rejected) = octet_ai::model_metadata::live_metadata_from_models_dev(&catalog)
        .map_err(anyhow::Error::msg)?;
    let cache = CacheFile {
        schema: CACHE_SCHEMA,
        octet_version: env!("CARGO_PKG_VERSION").to_owned(),
        fetched_at_ms: now_ms,
        etag,
        source_sha256: format!("{:x}", Sha256::digest(&body)),
        source_bytes: body.len() as u64,
        rejected,
        metadata,
    };
    write_cache(path, &cache)?;
    Ok(RefreshOutcome::Updated(cache.metadata))
}

async fn read_bounded(response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SOURCE_BYTES as u64)
    {
        anyhow::bail!("models.dev catalog exceeds the {MAX_SOURCE_BYTES}-byte limit");
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len() + chunk.len() > MAX_SOURCE_BYTES {
            anyhow::bail!("models.dev catalog exceeds the {MAX_SOURCE_BYTES}-byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests;

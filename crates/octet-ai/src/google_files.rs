//! Gemini Developer API resumable Files transport for recorded audio.
//! Contract: https://ai.google.dev/gemini-api/docs/files (resumable upload,
//! finalize, PROCESSING/ACTIVE states, 48-hour storage). Vertex uses GCS fileData,
//! not this API. Canonical session media always retains its original bytes.
use crate::{AiError, AudioMedia, AudioPayload, Media, Model, Protocol, ProviderMediaRef, Request};
use sha2::{Digest as _, Sha256};
use std::time::{Duration, SystemTime};

pub(crate) fn usable(reference: &ProviderMediaRef) -> bool {
    if !crate::validate::provider_ref_is_usable(reference, Protocol::GoogleGenerativeAi) {
        return false;
    }
    let Ok(url) = url::Url::parse(&reference.id) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    (url.scheme() == "https"
        && url.host_str() == Some("generativelanguage.googleapis.com")
        && url.port().is_none()
        && url.path().starts_with("/v1beta/files/"))
        || (url.scheme() == "gs" && url.host_str().is_some() && url.path().len() > 1)
}

pub(crate) fn upload_base(model: &Model) -> Option<url::Url> {
    let base = &model.endpoint.base_url;
    // Endpoint identity AND authority are required: a compatible gateway must
    // never acquire an unrelated provider's file service or credentials.
    (model.spec.protocol == Protocol::GoogleGenerativeAi
        && model.endpoint.id.0 == "gemini"
        && base.scheme() == "https"
        && base.host_str() == Some("generativelanguage.googleapis.com")
        && base.port().is_none()
        && base.path() == "/v1beta/")
        .then(|| {
            base.join("/upload/v1beta/files")
                .expect("fixed upload path")
        })
}

const FILE_URI_PREFIX: &str = "https://generativelanguage.googleapis.com/v1beta/files/";
const MAX_FILE_ID_BYTES: usize = 128;

const SLOT_PREFIX: &str = "octet-audio-slot:";

pub(crate) struct UploadSlots(Vec<AudioMedia>);

/// Admit the exact inference wire shape with worst-case file URI lengths.
/// Only audio selected by the codec acquires an upload slot. The private
/// placeholder scheme cannot be produced by a caller-supplied provider ref.
pub(crate) fn admit(
    model: &Model,
    request: &Request,
) -> Result<Option<(crate::protocol::HttpRequestParts, UploadSlots)>, AiError> {
    let estimated = crate::recorded_audio::estimated_media_bytes(request);
    if estimated < 16_000_000 {
        return Ok(None);
    }
    const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
    if estimated > MAX_SOURCE_BYTES {
        return Err(crate::recorded_audio::limit_error(model, MAX_SOURCE_BYTES));
    }
    crate::recorded_audio::check_projected_media_budget(model, request, |media| {
        matches!(media, Media::Audio(audio) if model.spec.supports_audio_input(audio.format)
            && matches!(audio.payload, AudioPayload::Inline(_)))
    })?;
    let mut slots = Vec::new();
    let parts = crate::protocol::google::build_request_with_audio(model, request, |audio| {
        let AudioPayload::Inline(data) = &audio.payload else {
            return Ok(crate::protocol::google::google_inline_audio_part(audio));
        };
        if data.len() > 20 * 1024 * 1024 {
            return Err(failure(model, CLIP_TOO_LARGE));
        }
        let index = slots.len();
        slots.push(audio.clone());
        let width = FILE_URI_PREFIX.len() + MAX_FILE_ID_BYTES - SLOT_PREFIX.len();
        Ok(Some(serde_json::json!({"fileData": {
            "mimeType": audio.mime_type(),
            "fileUri": format!("{SLOT_PREFIX}{index:0width$}"),
        }})))
    })?;
    Ok(Some((parts, UploadSlots(slots))))
}

impl UploadSlots {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub(crate) async fn resolve(
    http: &reqwest::Client,
    model: &Model,
    slots: UploadSlots,
    body: &bytes::Bytes,
    upload: &url::Url,
    headers: http::HeaderMap,
) -> Result<bytes::Bytes, AiError> {
    // Parse only our own admitted body, and replace only the exact file-data
    // leaves the codec reserved. Everything else stays as admitted, even if
    // another input reference would expire while an upload is in flight.
    let mut body: serde_json::Value = serde_json::from_slice(body).expect("built-in Google JSON");
    let mut digest = Sha256::new();
    digest.update(upload.as_str());
    for (name, value) in &headers {
        digest.update(name.as_str());
        digest.update([0]);
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    let identity: [u8; 32] = digest.finalize().into();
    let uris = tokio::time::timeout(Duration::from_secs(60), async {
        let mut uris = Vec::with_capacity(slots.0.len());
        for audio in slots.0 {
            uris.push(prepare_clip(http, model, &audio, upload, &headers, identity).await?);
        }
        Ok::<_, AiError>(uris)
    })
    .await
    .map_err(|_| {
        failure(
            model,
            "audio file preparation timed out; retry or use a smaller clip",
        )
    })??;
    for content in body["contents"].as_array_mut().expect("Google contents") {
        for part in content["parts"].as_array_mut().expect("Google parts") {
            if let Some(uri) = part
                .get_mut("fileData")
                .and_then(|data| data.get_mut("fileUri"))
            {
                if let Some(index) = uri.as_str().and_then(|uri| uri.strip_prefix(SLOT_PREFIX)) {
                    let index: usize = index.parse().expect("upload slot index");
                    *uri = serde_json::Value::String(uris[index].clone());
                }
            }
        }
    }
    // These ASCII provider URIs can never be longer than the admitted slots.
    crate::recorded_audio::encode(model, &body).map(bytes::Bytes::from)
}

fn failure(model: &Model, stage: &str) -> AiError {
    crate::ProviderError {
        code: Some("audio_file_preparation".into()),
        kind: None,
        request_id: None,
        message: format!(
            "provider {} model {}: {stage}",
            model.endpoint.id.0, model.spec.api_name
        ),
    }
    .into()
}

async fn metadata(
    response: reqwest::Response,
    model: &Model,
) -> Result<serde_json::Value, AiError> {
    if !response.status().is_success() {
        return Err(failure(
            model,
            "audio file service rejected the request; retry or use smaller inline clips",
        ));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| failure(model, "reading audio file metadata failed"))?
    {
        if bytes.len().saturating_add(chunk.len()) > 16 * 1024 {
            return Err(failure(
                model,
                "audio file metadata exceeds its safety bound",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| failure(model, "audio file service returned invalid metadata"))
}

const CLIP_TOO_LARGE: &str =
    "audio clip exceeds the 20 MiB safety bound; split or compress the original";

async fn prepare_clip(
    http: &reqwest::Client,
    model: &Model,
    audio: &AudioMedia,
    upload: &url::Url,
    headers: &http::HeaderMap,
    identity: [u8; 32],
) -> Result<String, AiError> {
    let AudioPayload::Inline(data) = &audio.payload else {
        unreachable!("only inline audio acquires an upload slot");
    };
    let data = data.clone();
    let cached = {
        let cache = audio
            .preparation
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .files
            .iter()
            .find(|(key, original, reference)| {
                key == &identity
                    && original.as_ptr() == data.as_ptr()
                    && original.len() == data.len()
                    && usable(reference)
            })
            .map(|(_, _, reference)| reference.clone())
    };
    if let Some(reference) = cached {
        return Ok(reference.id);
    }
    let response = http
        .post(upload.clone())
        .headers(headers.clone())
        .header("content-type", "application/json")
        .header("x-goog-upload-protocol", "resumable")
        .header("x-goog-upload-command", "start")
        .header("x-goog-upload-header-content-length", data.len())
        .header("x-goog-upload-header-content-type", audio.mime_type())
        .body("{\"file\":{\"display_name\":\"recorded-audio\"}}")
        .send()
        .await
        .map_err(|_| failure(model, "starting audio upload failed"))?;
    if !response.status().is_success() {
        return Err(failure(model, "starting audio upload was rejected"));
    }
    let destination = response
        .headers()
        .get("x-goog-upload-url")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| url::Url::parse(value).ok())
        .ok_or_else(|| failure(model, "audio upload destination is invalid"))?;
    if destination.origin() != upload.origin()
        || !destination.username().is_empty()
        || destination.password().is_some()
        || destination.fragment().is_some()
    {
        return Err(failure(model, "audio upload destination changed authority"));
    }
    let response = http
        .post(destination)
        .headers(headers.clone())
        .header("x-goog-upload-offset", "0")
        .header("x-goog-upload-command", "upload, finalize")
        .body(data.clone())
        .send()
        .await
        .map_err(|_| failure(model, "finalizing audio upload failed"))?;
    let mut record = metadata(response, model).await?;
    for attempt in 0..=20 {
        let file = record.get("file").unwrap_or(&record);
        let name = file
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| {
                name.strip_prefix("files/").is_some_and(|id| {
                    !id.is_empty()
                        && id.len() <= MAX_FILE_ID_BYTES
                        && id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
                        })
                })
            })
            .ok_or_else(|| failure(model, "audio file name is invalid"))?;
        match file.get("state").and_then(serde_json::Value::as_str) {
            Some("ACTIVE") => {
                let reference = ProviderMediaRef {
                    protocol: Protocol::GoogleGenerativeAi,
                    id: format!("{FILE_URI_PREFIX}{}", &name["files/".len()..]),
                    // Files have a documented 48h TTL. Revalidate/reupload much
                    // earlier and never persist this process-local optimization.
                    expires_at: Some(SystemTime::now() + Duration::from_secs(15 * 60)),
                };
                let mut cache = audio
                    .preparation
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if cache.files.len() == 4 {
                    cache.files.remove(0);
                }
                cache.files.push((identity, data, reference.clone()));
                return Ok(reference.id);
            }
            Some("PROCESSING") if attempt < 20 => {
                let mut poll = upload.clone();
                poll.set_path(&format!("/v1beta/{name}"));
                poll.set_query(None);
                tokio::time::sleep(Duration::from_millis(250)).await;
                record = metadata(
                    http.get(poll)
                        .headers(headers.clone())
                        .send()
                        .await
                        .map_err(|_| failure(model, "polling audio file preparation failed"))?,
                    model,
                )
                .await?;
            }
            _ => {
                return Err(failure(
                    model,
                    "audio file is not ready; retry or use a smaller clip",
                ))
            }
        }
    }
    unreachable!("bounded file-state loop always returns")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, UserPart};
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture() -> (Model, Request) {
        let mut model = crate::ModelCatalog::builtin()
            .unwrap()
            .resolve(&crate::ModelId("gpt-4o-mini".into()))
            .unwrap();
        Arc::make_mut(&mut model.spec).protocol = Protocol::GoogleGenerativeAi;
        Arc::make_mut(&mut model.spec).api_name = "gemini-file-test".into();
        Arc::make_mut(&mut model.spec).capabilities.input_modalities =
            crate::ModalitySet::none().with(crate::Modality::Audio);
        let request = Request {
            messages: vec![Message::User(crate::UserMessage {
                content: vec![
                    UserPart::Text("before".into()),
                    UserPart::Media(Media::audio_bytes(
                        bytes::Bytes::from(vec![42; 12_000_000]),
                        crate::AudioFormat::Wav,
                    )),
                    UserPart::Text("after".into()),
                ],
            })],
            ..crate::test_fixtures::base_request()
        };
        (model, request)
    }

    #[tokio::test]
    async fn resumable_audio_upload_reuses_files_by_credential_and_preserves_canonical_bytes() {
        let server = MockServer::start().await;
        let (model, canonical) = fixture();
        let upload = url::Url::parse(&format!("{}/upload/v1beta/files", server.uri())).unwrap();
        Mock::given(method("POST"))
            .and(path("/upload/v1beta/files"))
            .and(header("x-goog-upload-command", "start"))
            .respond_with(ResponseTemplate::new(200).insert_header(
                "x-goog-upload-url",
                format!("{}/upload-session", server.uri()),
            ))
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/upload-session"))
            .and(wiremock::matchers::headers(
                "x-goog-upload-command",
                vec!["upload", "finalize"],
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"file": {"name": "files/recorded", "state": "PROCESSING"}}),
            ))
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1beta/files/recorded"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"name": "files/recorded", "state": "ACTIVE"}),
                ),
            )
            .expect(3)
            .mount(&server)
            .await;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for key in [
            "first-private-key",
            "first-private-key",
            "replacement-private-key",
        ] {
            let mut headers = http::HeaderMap::new();
            headers.insert("x-goog-api-key", key.parse().unwrap());
            let (parts, slots) = admit(&model, &canonical).unwrap().unwrap();
            let body = resolve(&http, &model, slots, &parts.body, &upload, headers)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                value["contents"][0]["parts"],
                serde_json::json!([
                    {"text": "before"}, {"fileData": {"mimeType": "audio/wav", "fileUri": "https://generativelanguage.googleapis.com/v1beta/files/recorded"}}, {"text": "after"}
                ])
            );
            assert!(body.len() < 1024);
        }
        // Expiry is never a durable dependency: reconstruct from the saved bytes.
        let resumed: Request =
            serde_json::from_slice(&serde_json::to_vec(&canonical).unwrap()).unwrap();
        let (parts, slots) = admit(&model, &resumed).unwrap().unwrap();
        resolve(
            &http,
            &model,
            slots,
            &parts.body,
            &upload,
            http::HeaderMap::new(),
        )
        .await
        .unwrap();
        let Message::User(user) = &canonical.messages[0] else {
            unreachable!()
        };
        let UserPart::Media(Media::Audio(audio)) = &user.content[1] else {
            unreachable!()
        };
        assert!(
            matches!(&audio.payload, AudioPayload::Inline(data) if data.as_ref() == vec![42; 12_000_000])
        );
        for request in server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/upload-session")
        {
            assert_eq!(request.body, vec![42; 12_000_000]);
        }
    }

    #[tokio::test]
    async fn file_upload_rejects_changed_authority_without_sending_bytes_or_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-goog-upload-url", "https://untrusted.invalid/upload"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (model, request) = fixture();
        let (parts, slots) = admit(&model, &request).unwrap().unwrap();
        let error = resolve(
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            &model,
            slots,
            &parts.body,
            &url::Url::parse(&server.uri()).unwrap(),
            http::HeaderMap::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("openai")
                && error.contains("gemini-file-test")
                && error.contains("changed authority"),
            "{error}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[test]
    fn files_transport_is_only_selected_for_the_builtin_gemini_authority() {
        let (mut model, _) = fixture();
        Arc::make_mut(&mut model.endpoint).id.0 = "gemini".into();
        Arc::make_mut(&mut model.endpoint).base_url =
            url::Url::parse("https://generativelanguage.googleapis.com/v1beta/").unwrap();
        assert!(upload_base(&model).is_some());
        for endpoint in ["vertex", "opencode-google", "custom"] {
            Arc::make_mut(&mut model.endpoint).id.0 = endpoint.into();
            assert!(upload_base(&model).is_none());
        }
        Arc::make_mut(&mut model.endpoint).id.0 = "gemini".into();
        Arc::make_mut(&mut model.endpoint).base_url =
            url::Url::parse("https://other.invalid/v1beta/").unwrap();
        assert!(upload_base(&model).is_none());
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    #[test]
    fn file_references_require_the_right_protocol_authority_and_lifetime() {
        for (id, expected) in [
            (
                "https://generativelanguage.googleapis.com/v1beta/files/clip",
                true,
            ),
            ("gs://recordings/immutable-clip.wav", true),
            ("https://other.invalid/v1beta/files/clip", false),
            (
                "https://secret@generativelanguage.googleapis.com/v1beta/files/clip",
                false,
            ),
            (
                "https://generativelanguage.googleapis.com/v1beta/files/clip?key=secret",
                false,
            ),
            ("file:///tmp/clip.wav", false),
        ] {
            let mut reference = ProviderMediaRef {
                protocol: Protocol::GoogleGenerativeAi,
                id: id.into(),
                expires_at: Some(SystemTime::now() + Duration::from_secs(60)),
            };
            assert_eq!(usable(&reference), expected, "{id}");
            reference.expires_at = Some(SystemTime::now() - Duration::from_secs(1));
            assert!(!usable(&reference));
            reference.expires_at = None;
            reference.protocol = Protocol::OpenAiChat;
            assert!(!usable(&reference));
        }
    }
}

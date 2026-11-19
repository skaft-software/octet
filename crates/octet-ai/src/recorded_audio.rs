//! Bounded recorded-media request preparation. Canonical history retains bytes.
use crate::{AiError, AudioPayload, Media, Message, Model, Request, ToolResultPart, UserPart};

pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const GOOGLE_INLINE_BYTES: usize = 20_000_000;

pub(crate) fn limit_error(model: &Model, limit: usize) -> AiError {
    crate::ValidationError::RequestBytes {
        provider: model.endpoint.id.0.clone(),
        model: model.spec.api_name.clone(),
        limit,
    }
    .into()
}

fn media_bytes(media: &Media) -> usize {
    match media {
        Media::Audio(audio) => match &audio.payload {
            AudioPayload::Inline(data) | AudioPayload::InlineWithProviderRef { data, .. } => {
                data.len()
            }
            AudioPayload::ProviderRef(_) => 0,
        },
        Media::Image(image) => match &image.source {
            crate::ImageSource::Inline(data) => data.len(),
            _ => 0,
        },
        Media::Document(document) => document.data.len(),
        Media::Video(video) => video.data.len(),
    }
}

/// Preflight before constructing any base64 or JSON values. It bounds the
/// largest temporary media representation independently from token estimates.
pub(crate) fn estimated_media_bytes(request: &Request) -> usize {
    estimated_media_bytes_for(request, true, |_| false)
}

fn estimated_media_bytes_for(
    request: &Request,
    native_video: bool,
    replaced: impl Fn(&Media) -> bool,
) -> usize {
    let mut total = 0usize;
    let mut add = |media: &Media| {
        if replaced(media) {
            return;
        }
        let bytes = match media {
            Media::Video(video) if !native_video => {
                video
                    .cached_frames()
                    .map_or(crate::video::MAX_FRAME_BYTES, |frames| {
                        frames
                            .iter()
                            .map(|frame| match &frame.image.source {
                                crate::ImageSource::Inline(bytes) => bytes.len(),
                                _ => 0,
                            })
                            .sum()
                    })
            }
            _ => media_bytes(media),
        };
        total = total.saturating_add(bytes.div_ceil(3).saturating_mul(4));
    };
    for message in &request.messages {
        match message {
            Message::User(user) => {
                for part in &user.content {
                    match part {
                        UserPart::Media(media) => add(media),
                        UserPart::ToolResult(result) => {
                            for part in &result.content {
                                if let ToolResultPart::Media(media) = part {
                                    add(media);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Assistant(assistant) => {
                for part in &assistant.content {
                    if let crate::AssistantPart::Media(media) = part {
                        add(media);
                    }
                }
            }
        }
    }
    total
}

pub(crate) fn check_media_budget(model: &Model, request: &Request) -> Result<usize, AiError> {
    check_projected_media_budget(model, request, |_| false)
}

pub(crate) fn check_projected_media_budget(
    model: &Model,
    request: &Request,
    replaced: impl Fn(&Media) -> bool,
) -> Result<usize, AiError> {
    let total = estimated_media_bytes_for(request, model.spec.supports_native_video(), replaced);
    if total > MAX_REQUEST_BYTES {
        Err(limit_error(model, MAX_REQUEST_BYTES))
    } else {
        Ok(total)
    }
}

/// A writer ceiling bounds escaped text, schemas and encoded media together.
pub(crate) fn encode<T: serde::Serialize>(model: &Model, value: &T) -> Result<Vec<u8>, AiError> {
    encode_with_limit(
        model,
        value,
        if model.spec.protocol == crate::Protocol::GoogleGenerativeAi {
            GOOGLE_INLINE_BYTES
        } else {
            MAX_REQUEST_BYTES
        },
    )
}

fn encode_with_limit<T: serde::Serialize>(
    model: &Model,
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, AiError> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("request byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|error| {
        if error.is_io() {
            limit_error(model, limit)
        } else {
            crate::DecodeError::Json("request serialization failed".into()).into()
        }
    })?;
    Ok(writer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoded_media_is_shared_but_not_durable_or_stale_after_payload_replacement() {
        let Media::Audio(mut audio) = Media::audio_bytes(
            bytes::Bytes::from_static(b"original"),
            crate::AudioFormat::Wav,
        ) else {
            unreachable!()
        };
        let encoded = audio.encoded().unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &encoded,
            &audio.clone().encoded().unwrap()
        ));
        let resumed: crate::AudioMedia =
            serde_json::from_slice(&serde_json::to_vec(&audio).unwrap()).unwrap();
        assert_eq!(resumed.encoded().unwrap(), encoded);
        assert!(!std::sync::Arc::ptr_eq(
            &encoded,
            &resumed.encoded().unwrap()
        ));
        audio.payload = AudioPayload::Inline(bytes::Bytes::from_static(b"replacement"));
        assert_ne!(encoded, audio.encoded().unwrap());
    }
    #[test]
    fn encoded_request_budget_counts_json_escaping_and_names_route() {
        let model = crate::ModelCatalog::builtin()
            .unwrap()
            .resolve(&crate::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let error = encode_with_limit(&model, &"\0".repeat(10), 20)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("openai") && error.contains("gpt-4o-mini"),
            "{error}"
        );
        assert!(error.contains("fewer or smaller"), "{error}");
        assert_eq!(encode_with_limit(&model, &"123", 5).unwrap(), b"\"123\"");
    }
    #[test]
    fn aggregate_media_budget_rejects_before_any_audio_encoding() {
        let model = crate::ModelCatalog::builtin()
            .unwrap()
            .resolve(&crate::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let media = Media::audio_bytes(
            bytes::Bytes::from(vec![0; 20 * 1024 * 1024]),
            crate::AudioFormat::Wav,
        );
        let request = Request {
            messages: vec![Message::User(crate::UserMessage {
                content: vec![UserPart::Media(media.clone()); 3],
            })],
            ..crate::test_fixtures::base_request()
        };
        assert!(check_media_budget(&model, &request).is_err());
        let Media::Audio(audio) = media else {
            unreachable!()
        };
        assert!(audio.preparation.inner.lock().unwrap().encoded.is_none());
    }
}

#[cfg(test)]
mod durable_media_tests {
    use super::*;
    #[test]
    fn image_and_pdf_serialization_share_preparation_without_changing_durable_shape() {
        for media in [
            Media::image_bytes(bytes::Bytes::from_static(b"picture"), mime::IMAGE_PNG),
            Media::pdf_bytes(
                bytes::Bytes::from_static(b"%PDF-original"),
                Some("score.pdf".into()),
            ),
        ] {
            let preparation = match &media {
                Media::Image(image) => &image.preparation,
                Media::Document(document) => &document.preparation,
                _ => unreachable!(),
            };
            assert!(preparation.inner.lock().unwrap().encoded.is_none());
            let first = serde_json::to_value(&media).unwrap();
            let encoded = preparation
                .inner
                .lock()
                .unwrap()
                .encoded
                .as_ref()
                .unwrap()
                .1
                .clone();
            assert_eq!(serde_json::to_value(media.clone()).unwrap(), first);
            assert!(std::sync::Arc::ptr_eq(
                &encoded,
                &preparation
                    .inner
                    .lock()
                    .unwrap()
                    .encoded
                    .as_ref()
                    .unwrap()
                    .1
            ));
            let resumed: Media = serde_json::from_value(first.clone()).unwrap();
            assert_eq!(serde_json::to_value(&resumed).unwrap(), first);
        }
    }
}

//! Typed media loading at the host boundary.
//!
//! This module resolves and bounds user-supplied media while preserving input
//! order and enforcing workspace confinement and model-effective modalities.

use std::path::Path;

use anyhow::Context as _;
use octet_agent::{InputPart, UserInput};
use octet_ai::{AudioFormat, Media, Modality, ModelSpec};

use super::protocol::{
    MediaInput, RunRequest, MAX_AUDIO_BYTES, MAX_IMAGE_BYTES, MAX_TOTAL_AUDIO_BYTES,
    MAX_TOTAL_IMAGE_BYTES,
};

pub(crate) fn load_user_input(
    request: &RunRequest,
    prompt: String,
    model: &ModelSpec,
) -> anyhow::Result<UserInput> {
    let media = if request.media.is_empty() {
        request
            .image_paths
            .iter()
            .cloned()
            .map(|path| MediaInput::Image { path })
            .collect::<Vec<_>>()
    } else {
        request.media.clone()
    };
    let mut parts = Vec::with_capacity(media.len() + 1);
    parts.push(InputPart::Text(prompt));
    if media.is_empty() {
        return Ok(UserInput::from(parts));
    }

    let workspace = request
        .workspace
        .canonicalize()
        .with_context(|| format!("resolving workspace {}", request.workspace.display()))?;
    let effective_modalities = model.effective_input_modalities();
    let mut total_image_bytes = 0u64;
    let mut total_audio_bytes = 0u64;
    for input in media {
        let (kind, path) = match &input {
            MediaInput::Image { path } => ("image", path),
            MediaInput::Audio { path } => ("audio", path),
        };
        let resolved = path
            .canonicalize()
            .with_context(|| format!("resolving {kind} {}", path.display()))?;
        if !request.allow_external_paths && !resolved.starts_with(&workspace) {
            anyhow::bail!(
                "{kind} {} is outside the configured workspace",
                resolved.display()
            );
        }

        let media = match input {
            MediaInput::Image { .. } => {
                if !effective_modalities.contains(Modality::Image) {
                    anyhow::bail!("model {} does not support native image input", model.id.0);
                }
                let mime = image_mime(&resolved)?;
                let bytes = octet_agent::secure_fs::read_regular_file_bounded(
                    &resolved,
                    MAX_IMAGE_BYTES as usize,
                )
                .with_context(|| format!("reading image {}", resolved.display()))?;
                total_image_bytes = total_image_bytes
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| anyhow::anyhow!("image byte size overflow"))?;
                if total_image_bytes > MAX_TOTAL_IMAGE_BYTES {
                    anyhow::bail!("images exceed the {MAX_TOTAL_IMAGE_BYTES}-byte total limit");
                }
                Media::image_bytes(bytes::Bytes::from(bytes), mime)
            }
            MediaInput::Audio { .. } => {
                let format = audio_format(&resolved).with_context(|| {
                    format!(
                        "native audio attachment {} must use WAV or MP3",
                        resolved.display()
                    )
                })?;
                if !model.supports_audio_input(format) {
                    anyhow::bail!(
                        "model {} does not support native {} audio input through {:?}; native input accepts only WAV or MP3 on OpenAI Chat Completions",
                        model.id.0,
                        audio_format_name(format),
                        model.protocol
                    );
                }
                let bytes = octet_agent::secure_fs::read_regular_file_bounded(
                    &resolved,
                    MAX_AUDIO_BYTES as usize,
                )
                .with_context(|| format!("reading audio {}", resolved.display()))?;
                total_audio_bytes = total_audio_bytes
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| anyhow::anyhow!("audio byte size overflow"))?;
                if total_audio_bytes > MAX_TOTAL_AUDIO_BYTES {
                    anyhow::bail!("audio exceeds the {MAX_TOTAL_AUDIO_BYTES}-byte total limit");
                }
                Media::audio_bytes(bytes::Bytes::from(bytes), format)
            }
        };
        parts.push(InputPart::Media(media));
    }
    Ok(UserInput::from(parts))
}

pub(crate) fn image_mime(path: &Path) -> anyhow::Result<mime::Mime> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => Ok(mime::IMAGE_PNG),
        "jpg" | "jpeg" => Ok(mime::IMAGE_JPEG),
        "gif" => Ok(mime::IMAGE_GIF),
        "webp" => Ok("image/webp".parse().expect("static MIME is valid")),
        _ => anyhow::bail!("unsupported image extension for {}", path.display()),
    }
}

pub(crate) fn audio_format(path: &Path) -> anyhow::Result<AudioFormat> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "wav" => Ok(AudioFormat::Wav),
        "mp3" => Ok(AudioFormat::Mp3),
        "flac" => Ok(AudioFormat::Flac),
        "opus" | "ogg" => Ok(AudioFormat::Opus),
        "aac" | "m4a" => Ok(AudioFormat::Aac),
        "pcm" | "pcm16" => Ok(AudioFormat::Pcm16),
        _ => anyhow::bail!("unsupported audio extension for {}", path.display()),
    }
}
pub(crate) fn audio_format_name(format: AudioFormat) -> &'static str {
    match format {
        AudioFormat::Wav => "WAV",
        AudioFormat::Aac => "AAC",
        AudioFormat::Mp3 => "MP3",
        AudioFormat::Flac => "FLAC",
        AudioFormat::Opus => "Opus",
        AudioFormat::Pcm16 => "PCM16",
    }
}

#[cfg(test)]
mod tests;

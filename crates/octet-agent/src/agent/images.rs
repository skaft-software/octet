//! User image admission: limits, cancellation and message assembly.

use super::*;
use crate::session::{CustomMessageContent, CustomMessagePart};
use base64::Engine as _;

/// Fallback when the model has no declared image bounds. This is a host safety
/// ceiling, not a claim about what any particular provider accepts.
pub(super) const FALLBACK_IMAGE_LIMITS: ImageInputLimits = ImageInputLimits {
    max_width: 4_000,
    max_height: 4_000,
    max_bytes: octet_ai::MAX_USER_IMAGE_BYTES,
};

// Also bounds aggregate decode work: each of at most eight images is subject
// to octet-ai's 16-million-pixel and bounded-resize limits.
pub(super) const MAX_IMAGES_PER_INPUT: usize = 8;

pub(super) const MAX_IMAGE_INPUT_BYTES: usize = 20 * 1024 * 1024;

/// Incremental position in the selected canonical context or Responses replay.
#[derive(Clone, Copy)]
pub(super) struct VideoPreparationCursor {
    generation: u64,
    replay: bool,
    limits: Option<ImageInputLimits>,
    pub(super) messages: usize,
}

/// Copy only video handles from the active window's changed suffix. Never
/// traverse raw entries: abandoned branches and summarized prefixes are durable
/// history, not decoder input. Revisit the tail because tool results coalesce.
pub(super) fn session_video_candidates(
    session: &Session,
    model: &Model,
    previous: Option<VideoPreparationCursor>,
) -> Result<(VideoPreparationCursor, Vec<Message>), AgentError> {
    let replay = if model.spec.protocol == Protocol::OpenAiResponses {
        session.responses_replay_snapshot(&model.endpoint.id, &model.spec.id)?
    } else {
        None
    };
    let generation = session.context_generation();
    let start = previous
        .filter(|cursor| {
            cursor.generation == generation
                && cursor.replay == replay.is_some()
                && cursor.limits == model.spec.preset.image_input_limits
        })
        .map_or(0, |cursor| cursor.messages.saturating_sub(1));
    let mut content = Vec::new();
    let mut add = |user: &UserMessage| {
        content.extend(
            lowered_tool_result_media(user)
                .filter(|media| matches!(media, Media::Video(_)))
                .cloned()
                .map(UserPart::Media),
        );
    };
    let end = if let Some(replay) = &replay {
        for item in &replay[start..] {
            if let ResponsesReplayItem::User(user) = item {
                add(user);
            }
        }
        replay.len()
    } else {
        let context = session.context_ref()?;
        for message in &context[start..] {
            if let Message::User(user) = message {
                add(user);
            }
        }
        context.len()
    };
    Ok((
        VideoPreparationCursor {
            generation,
            replay: replay.is_some(),
            limits: model.spec.preset.image_input_limits,
            messages: end,
        },
        vec![Message::User(UserMessage { content })],
    ))
}

/// Stop a detached blocking decoder at the next image when its async owner ends.
pub(super) struct CancelBlockingImages(pub(super) Arc<AtomicBool>);

impl Drop for CancelBlockingImages {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Check the entire batch before spawning any decode work or appending history.
pub(super) fn image_preparation_limits(
    input: &UserInput,
    model: &Model,
) -> Result<ImageInputLimits, AgentError> {
    for custom in &input.custom_messages {
        custom.validate()?;
    }
    let custom_parts: Vec<InputPart> = input
        .custom_messages
        .iter()
        .flat_map(|custom| custom.content.input_parts())
        .collect();
    let mut count = 0usize;
    let mut bytes = 0usize;
    let mut videos = 0;
    for part in input.parts.iter().chain(&custom_parts) {
        if let InputPart::Media(Media::Video(video)) = part {
            octet_ai::video::validate_video(video)?;
            if !model.spec.supports_video_input() {
                return Err(AiError::Unsupported(octet_ai::UnsupportedError::Video).into());
            }
            videos += 1;
            count += octet_ai::video::VIDEO_FRAMES;
            bytes = bytes.saturating_add(video.data.len());
            if videos > 2 || count > MAX_IMAGES_PER_INPUT || bytes > MAX_IMAGE_INPUT_BYTES {
                return Err(AgentError::ImageInputBatchLimit);
            }
        }
        if let InputPart::Media(Media::Image(image)) = part {
            count += 1;
            if let ImageSource::Inline(data) = &image.source {
                if data.len() > octet_ai::MAX_USER_IMAGE_BYTES {
                    return Err(ImageInputError::InputTooLarge.into());
                }
                bytes = bytes.saturating_add(data.len());
            }
            if count > MAX_IMAGES_PER_INPUT || bytes > MAX_IMAGE_INPUT_BYTES {
                return Err(AgentError::ImageInputBatchLimit);
            }
        }
    }
    if count > videos * octet_ai::video::VIDEO_FRAMES
        && !model
            .spec
            .effective_input_modalities()
            .contains(Modality::Image)
    {
        return Err(AiError::Unsupported(octet_ai::UnsupportedError::Image).into());
    }
    let limits = model
        .spec
        .preset
        .image_input_limits
        .unwrap_or(FALLBACK_IMAGE_LIMITS);
    if count > 0 {
        limits.validate()?;
    }
    Ok(limits)
}

/// Transform canonical input off the async worker, atomically before history
/// append. Cancellation stops between images and never commits a partial batch.
pub(super) async fn prepare_user_images(
    mut input: UserInput,
    model: &Model,
    abort: Option<&AbortFlag>,
) -> Result<UserInput, AgentError> {
    let limits = image_preparation_limits(&input, model)?;
    if abort.is_some_and(AbortFlag::is_set) {
        return Err(AgentError::Cancelled);
    }
    if !input.parts.iter().any(|part| {
        matches!(part,
            InputPart::Media(Media::Image(image)) if matches!(image.source, ImageSource::Inline(_))
        ) || matches!(part, InputPart::Media(Media::Video(_)))
    }) && !input.custom_messages.iter().any(|custom| matches!(&custom.content,
        CustomMessageContent::Parts(parts) if parts.iter().any(|part| matches!(part, CustomMessagePart::Image { .. })))) {
        return Ok(input);
    }
    if !model.spec.supports_native_video() {
        for part in &input.parts {
            if let InputPart::Media(Media::Video(video)) = part {
                let preparation = video.prepare_frames();
                if let Some(abort) = abort {
                    tokio::select! {
                        biased;
                        _ = abort.wait() => return Err(AgentError::Cancelled),
                        result = preparation => { result?; }
                    }
                } else {
                    preparation.await?;
                }
            }
        }
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancelBlockingImages(Arc::clone(&cancelled));
    let worker = tokio::task::spawn_blocking(move || {
        for part in &mut input.parts {
            if cancelled.load(Ordering::Acquire) {
                return Err(AgentError::Cancelled);
            }
            if let InputPart::Media(Media::Image(image)) = part {
                *image = octet_ai::prepare_user_image(image, limits)?;
            }
        }
        for custom in &mut input.custom_messages {
            if let CustomMessageContent::Parts(parts) = &mut custom.content {
                for part in parts {
                    if cancelled.load(Ordering::Acquire) {
                        return Err(AgentError::Cancelled);
                    }
                    if let Some(image) = part.image()? {
                        let prepared = octet_ai::prepare_user_image(&image, limits)?;
                        let ImageSource::Inline(bytes) = prepared.source else {
                            unreachable!("inline preparation");
                        };
                        *part = CustomMessagePart::Image {
                            data: base64::engine::general_purpose::STANDARD.encode(bytes),
                            mime_type: prepared
                                .media_type
                                .expect("prepared image MIME")
                                .to_string(),
                        };
                    }
                }
            }
        }
        Ok(input)
    });
    let result = if let Some(abort) = abort {
        tokio::select! {
            biased;
            _ = abort.wait() => Err(AgentError::Cancelled),
            result = worker => result.map_err(|_| AgentError::ImagePreparationFailed)?,
        }
    } else {
        worker
            .await
            .map_err(|_| AgentError::ImagePreparationFailed)?
    };
    drop(guard);
    if abort.is_some_and(AbortFlag::is_set) {
        return Err(AgentError::Cancelled);
    }
    result
}

pub(super) fn user_message(input: UserInput) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: input.into_user_parts(),
    }))
}

impl Agent {
    /// Commit idle extension input using the same native image admission as a prompt,
    /// without starting inference or creating a checkpoint.
    pub async fn append_idle_input(
        &mut self,
        input: UserInput,
    ) -> Result<crate::session::EntryId, AgentError> {
        let input = prepare_user_images(input, &self.model, None).await?;
        Ok(input.append_to(&mut self.session, None)?)
    }
}

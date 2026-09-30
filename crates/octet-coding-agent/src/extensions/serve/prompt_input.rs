//! Resolving prompt and control input: attachments, stored media and resources.

use super::*;

pub(super) fn resolve_attachment_media(
    app: &App,
    plan: &WorkerPlan,
    references: &[AttachmentRef],
) -> Result<Vec<Media>, ServiceError> {
    let supports_images = app
        .model
        .spec
        .capabilities
        .input_modalities
        .contains(Modality::Image);
    resolve_stored_media(supports_images, plan.attachments.as_ref(), references)
}

pub(super) fn resolve_stored_media(
    supports_images: bool,
    store: Option<&AttachmentStore>,
    references: &[AttachmentRef],
) -> Result<Vec<Media>, ServiceError> {
    if references.is_empty() {
        return Ok(Vec::new());
    }
    if !supports_images {
        return Err(ServiceError::InvalidBoundary);
    }
    let store = store.ok_or(ServiceError::Unavailable)?;
    let resolved = store
        .resolve_many(references)
        .map_err(attachment_service_error)?;
    resolved
        .into_iter()
        .map(|attachment| {
            let media_type = attachment
                .reference
                .media_type
                .parse()
                .map_err(|_| ServiceError::InvalidBoundary)?;
            Ok(Media::image_bytes(attachment.bytes, media_type))
        })
        .collect()
}

pub(super) fn token_hint_for_bytes(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

pub(super) fn project_instruction_token_hint(system: &str) -> u64 {
    const START: &str = "<project_context>\n";
    const END: &str = "\n</project_context>";

    let Some(start) = system.find(START) else {
        return 0;
    };
    let section_start = start.saturating_add(START.len());
    let Some(relative_end) = system[section_start..].find(END) else {
        return 0;
    };
    token_hint_for_bytes(relative_end)
}

pub(super) async fn resolve_prompt_input(
    plan: &WorkerPlan,
    input: PromptInput,
) -> Result<ResolvedPromptInput, ServiceError> {
    let PromptInput {
        text,
        attachments,
        document_ids,
        project_file_ids,
    } = input;
    let project_id = plan.project_id.as_ref();
    let document_context = if document_ids.is_empty() {
        None
    } else {
        let project_id = project_id.ok_or(ServiceError::Unauthorized)?.clone();
        let session_id = plan.session_id.clone();
        let store = plan.documents.clone().ok_or(ServiceError::Unavailable)?;
        Some(
            tokio::task::spawn_blocking(move || {
                store.prompt_context(project_id.as_str(), session_id.as_str(), &document_ids)
            })
            .await
            .map_err(|_| ServiceError::Internal)?
            .map_err(document_store_service_error)?,
        )
    };
    let project_file_context = if project_file_ids.is_empty() {
        None
    } else {
        let project_id = project_id.ok_or(ServiceError::Unauthorized)?.clone();
        let projects = Arc::clone(&plan.projects);
        let trusted_files = Arc::clone(&plan.trusted_files);
        Some(
            tokio::task::spawn_blocking(move || {
                with_trusted_project_files(
                    &projects,
                    &trusted_files,
                    &project_id,
                    |service, registry| service.attach_as_context(registry, &project_file_ids),
                )
            })
            .await
            .map_err(|_| ServiceError::Internal)??,
        )
    };
    let composed = octet_serve_backend::compose_prompt_text(
        &text,
        document_context
            .as_ref()
            .map(|context| context.text.as_str()),
        project_file_context
            .as_ref()
            .map(|context| context.text.as_str()),
    )
    .map_err(|error| match error {
        octet_serve_backend::PromptContextError::InvalidUserText
        | octet_serve_backend::PromptContextError::InvalidDocumentContext
        | octet_serve_backend::PromptContextError::InvalidProjectFileContext => {
            ServiceError::InvalidBoundary
        }
        octet_serve_backend::PromptContextError::DocumentContextTooLarge
        | octet_serve_backend::PromptContextError::ProjectFileContextTooLarge
        | octet_serve_backend::PromptContextError::AuxiliaryContextTooLarge
        | octet_serve_backend::PromptContextError::PromptTooLarge => ServiceError::PayloadTooLarge,
    })?;
    let document_context_tokens = token_hint_for_bytes(composed.document_context_bytes());
    let project_file_context_tokens = token_hint_for_bytes(composed.project_file_context_bytes());
    Ok(ResolvedPromptInput {
        display_text: text,
        model_text: composed.into_string(),
        attachments,
        documents: document_context
            .map(|context| context.documents)
            .unwrap_or_default(),
        project_files: project_file_context
            .map(|context| context.files)
            .unwrap_or_default(),
        document_context_tokens,
        project_file_context_tokens,
    })
}

pub(super) fn resolve_control_input(
    plan: &WorkerPlan,
    text: String,
    references: &[AttachmentRef],
) -> Result<UserInput, ServiceError> {
    let mut parts = Vec::with_capacity(1 + references.len());
    if !text.is_empty() {
        parts.push(InputPart::Text(text));
    }
    let supports_images = plan
        .available_models
        .iter()
        .find(|summary| summary.id == plan.launch.model.0)
        .is_some_and(|summary| summary.input_modalities.contains(&InputModality::Image));
    parts.extend(
        resolve_stored_media(supports_images, plan.attachments.as_ref(), references)?
            .into_iter()
            .map(InputPart::Media),
    );
    Ok(UserInput::from(parts))
}

pub(super) fn attachment_service_error(error: AttachmentError) -> ServiceError {
    match error {
        AttachmentError::Unavailable | AttachmentError::QuotaExceeded => ServiceError::Unavailable,
        AttachmentError::Storage => ServiceError::Internal,
        AttachmentError::InvalidName
        | AttachmentError::UnsupportedMediaType
        | AttachmentError::InvalidContent
        | AttachmentError::TooLarge
        | AttachmentError::NotFound
        | AttachmentError::MetadataMismatch => ServiceError::InvalidBoundary,
    }
}

pub(super) fn resource_store_service_error(
    error: octet_serve_backend::ResourceStoreError,
) -> ServiceError {
    match error {
        octet_serve_backend::ResourceStoreError::InvalidBoundary => ServiceError::InvalidBoundary,
        octet_serve_backend::ResourceStoreError::QuotaExceeded => ServiceError::Unavailable,
        octet_serve_backend::ResourceStoreError::NotFound => ServiceError::NotFound,
        octet_serve_backend::ResourceStoreError::Corrupt => ServiceError::CorruptResource,
        octet_serve_backend::ResourceStoreError::Storage => ServiceError::Internal,
    }
}

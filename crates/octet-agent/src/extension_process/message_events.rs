use super::*;

pub(super) fn pi_execution_result(output: &ToolOutput) -> Option<serde_json::Value> {
    let mut content = Vec::new();
    for part in output.content_parts() {
        match part {
            ToolOutputContentPart::Text(text) => {
                content.push(serde_json::json!({"type":"text", "text":text}))
            }
            ToolOutputContentPart::Media(Media::Image(image)) => {
                let (octet_ai::ImageSource::Inline(data), Some(mime)) =
                    (&image.source, &image.media_type)
                else {
                    return None;
                };
                content.push(serde_json::json!({"type":"image", "mimeType":mime.to_string(), "data":base64::engine::general_purpose::STANDARD.encode(data)}));
            }
            _ => return None,
        }
    }
    let mut result = serde_json::json!({"content":content});
    if let Some(details) = output
        .metadata()
        .and_then(|metadata| metadata.get("pi_details"))
    {
        result["details"] = details.clone();
    }
    if let Some(usage) = output
        .metadata()
        .and_then(|metadata| metadata.get("pi_usage"))
    {
        result["usage"] = usage.clone();
    }
    if let Some(value) = output.structured_content() {
        result["structuredContent"] = value.clone();
    }
    Some(result)
}

impl ExtensionProcess {
    pub(super) fn flush_pi_message_deltas(&self, endpoint: &LifecycleEndpoint, owner: &str) {
        let (batches, message_id) = {
            let mut coalescer = lock_std_mutex(&endpoint.connection.message_deltas);
            let batches = coalescer.flush(Instant::now());
            (batches, coalescer.active_message_id())
        };
        for batch in batches {
            let Ok(mut params) = serde_json::to_value(batch.into_updated(message_id.clone()))
            else {
                continue;
            };
            params["resource_owner"] = serde_json::json!({"session_id":owner,"extension_instance_id":self.inner.instance_id,"process_generation":endpoint.generation});
            endpoint
                .connection
                .queue_notification(methods::MESSAGE_UPDATED, params);
        }
    }

    pub(super) fn queue_pi_tool_event(
        &self,
        endpoint: &LifecycleEndpoint,
        event: ExtensionLifecycleEvent,
        owner: &str,
        fields: serde_json::Value,
    ) -> Result<bool, ExtensionRuntimeError> {
        let protocol = read_std_lock(&endpoint.connection.protocol).clone();
        if !protocol.supports(EXTENSION_FEATURE_COMMITTED_MESSAGE_EVENTS_V1) {
            return Self::queue_lifecycle_observation(endpoint, event);
        }
        if !protocol.supports(EXTENSION_FEATURE_LIFECYCLE_EVENTS)
            || !protocol.lifecycle_events.contains(event.method())
        {
            return Ok(false);
        }
        let mut params = event.params()?;
        params
            .as_object_mut()
            .expect("lifecycle object")
            .extend(fields.as_object().expect("tool fields").clone());
        params["resource_owner"] = serde_json::json!({"session_id":owner, "extension_instance_id":self.inner.instance_id, "process_generation":endpoint.generation});
        Ok(endpoint
            .connection
            .queue_notification(event.method(), params))
    }
}

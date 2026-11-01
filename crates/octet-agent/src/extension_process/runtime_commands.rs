use super::*;

#[allow(missing_docs)]
pub const EXTENSION_FEATURE_RUNTIME_COMMAND_REGISTRATION_V1: &str =
    "runtime_command_registration_v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRegistration {
    parent_request_id: u64,
    #[serde(default)]
    resource_owner: Option<ExtensionResourceOwner>,
    commands: Vec<CommandDefinition>,
}

impl OwnerScopedHostRequest for CommandRegistration {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }

    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        self.resource_owner.as_ref()
    }
}

pub(super) fn register_runtime_commands(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let id = parse_child_request_id(object, "commands/register")?;
    let negotiated = read_std_lock(&state.protocol);
    let supported = negotiated.version == EXTENSION_API_VERSION_0_4
        && negotiated.supports(EXTENSION_FEATURE_RUNTIME_COMMANDS)
        && negotiated.supports(EXTENSION_FEATURE_RUNTIME_COMMAND_REGISTRATION_V1);
    drop(negotiated);
    if !supported {
        return reject_typed_child_request(state, id, ExtensionRequestFailure::UnsupportedFeature,
            "commands/register requires API 0.4 runtime_commands and runtime_command_registration_v1");
    }
    if let Err(detail) = validate_remote_ui_envelope(object, true) {
        return reject_typed_child_request(
            state,
            id,
            ExtensionRequestFailure::InvalidRequest,
            detail,
        );
    }
    let Some((request, admitted)) = admit_host_request::<CommandRegistration>(
        state,
        object,
        "commands/register",
        EXTENSION_FEATURE_RUNTIME_COMMAND_REGISTRATION_V1,
        params,
    )?
    else {
        return Ok(());
    };
    let invalid = |detail: String| (ExtensionRequestFailure::InvalidRequest, detail);
    let validation = (|| {
        if request.commands.is_empty() || request.commands.len() > MAX_EXTENSION_COMMANDS {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                "command batch must contain 1..=256 definitions".into(),
            ));
        }
        let mut names = BTreeSet::new();
        for command in &request.commands {
            validate_identifier("command", &command.name, true)
                .map_err(|error| invalid(error.to_string()))?;
            if !names.insert(&command.name) {
                return Err(invalid(format!(
                    "duplicate command definition `{}`",
                    command.name
                )));
            }
            if command.description.trim().is_empty() {
                return Err(invalid(format!(
                    "command `{}` has an empty description",
                    command.name
                )));
            }
            bounded_plain_text_failure("command description", &command.description, 4096)?;
            if let Some(usage) = &command.usage {
                bounded_plain_text_failure("command usage", usage, 4096)?;
            }
        }
        validate_explicit_request_owner(state, &admitted.owner)?;
        if request
            .resource_owner
            .as_ref()
            .is_some_and(|owner| owner != &admitted.owner)
            || lock_std_mutex(&state.tombstones).contains(request.parent_request_id)
        {
            return Err((
                ExtensionRequestFailure::NotForegroundOwner,
                "command registration owner or parent is stale".into(),
            ));
        }
        Ok(())
    })();
    if let Err(failure) = validation {
        return refuse_admitted_request(state, &admitted, failure);
    }
    let permit = match state.writer.try_reserve() {
        Ok(permit) => permit,
        Err(error) => {
            settle_child_request(&state.child_requests, &admitted.request_id);
            return Err(format!("command registration writer unavailable: {error}"));
        }
    };
    let committed = (|| {
        let pending = lock_std_mutex(&state.pending);
        let owners = lock_std_mutex(&state.issued_resource_owners);
        let stale = || {
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "command registration owner, parent or generation is no longer active".into(),
            )
        };
        if state.closed.load(Ordering::Acquire)
            || state.draining.load(Ordering::Acquire)
            || !owners.contains(&admitted.owner)
        {
            return Err(stale());
        }
        let mut catalog = write_std_lock(&state.command_catalog);
        let mut prospective = catalog.clone();
        for command in request.commands {
            if let Some(existing) = prospective
                .iter_mut()
                .find(|existing| existing.name == command.name)
            {
                *existing = command;
            } else {
                prospective.push(command);
            }
        }
        if prospective.len() > MAX_EXTENSION_COMMANDS {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                "command catalog exceeds 256 definitions".into(),
            ));
        }
        let mut line = serde_json::to_vec(&serde_json::json!({
            "jsonrpc":"2.0", "id":admitted.request_id,
            "result":{"commands":prospective.iter().map(|command| &command.name).collect::<Vec<_>>()},
        })).expect("command registration acknowledgement is JSON");
        line.push(b'\n');
        if line.len() > state.max_message_bytes() {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                "command acknowledgement exceeds transport limit".into(),
            ));
        }
        let mut children = lock_std_mutex(&state.child_requests);
        let child = children.get(&admitted.request_id).ok_or_else(stale)?;
        if child.parent_request_id != 0
            && pending
                .get(&child.parent_request_id)
                .is_none_or(|parent| parent.terminal.load(Ordering::Acquire) != REQUEST_ACTIVE)
        {
            return Err(stale());
        }
        let response = Arc::clone(&child.response_state);
        response
            .state
            .compare_exchange(
                CHILD_ACTIVE,
                CHILD_RESPONDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| stale())?;
        *catalog = prospective;
        response.state.store(CHILD_SETTLED, Ordering::Release);
        children.remove(&admitted.request_id);
        response.changed.notify_waiters();
        permit.send(WriterFrame {
            line,
            state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
            completion: None,
            bus_delivery: None,
        });
        Ok(())
    })();
    match committed {
        Ok(()) => Ok(()),
        Err(failure) => refuse_admitted_request(state, &admitted, failure),
    }
}

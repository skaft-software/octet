use super::*;
use crate::secure_fs::{self, PreparedMutation, SecureFileError};
use serde_json::{json, Map, Value};

pub(super) const FEATURE: &str = "pi_settings_v1";
const LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PiSettingsConfig {
    pub workspace: PathBuf,
    pub agent_dir: PathBuf,
    pub project_trusted: bool,
}

pub(super) struct Service {
    config: PiSettingsConfig,
    parents: StdMutex<HashMap<u64, ExtensionResourceOwner>>,
}

impl Service {
    pub(super) fn new(config: PiSettingsConfig) -> Self {
        Self {
            config,
            parents: StdMutex::new(HashMap::new()),
        }
    }

    pub(super) fn remember_parent(
        &self,
        id: u64,
        owner: &ExtensionResourceOwner,
        issued: &HashSet<ExtensionResourceOwner>,
    ) -> Result<(), ExtensionRuntimeError> {
        let mut parents = lock_std_mutex(&self.parents);
        parents.retain(|_, owner| issued.contains(owner));
        if parents.len() >= 4096 {
            return Err(ExtensionRuntimeError::Protocol(
                "Pi settings retained-parent quota exhausted".into(),
            ));
        }
        parents.insert(id, owner.clone());
        Ok(())
    }

    pub(super) fn forget_parent(&self, id: u64) {
        lock_std_mutex(&self.parents).remove(&id);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsRequest {
    parent_request_id: u64,
    #[serde(default)]
    resource_owner: Option<ExtensionResourceOwner>,
    cwd: PathBuf,
    agent_dir: PathBuf,
    project_trusted: bool,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    extensions: Option<Vec<String>>,
}

impl OwnerScopedHostRequest for SettingsRequest {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }

    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        self.resource_owner.as_ref()
    }
}

type Failure = (ExtensionRequestFailure, String);

fn invalid(detail: impl Into<String>) -> Failure {
    (ExtensionRequestFailure::InvalidRequest, detail.into())
}

fn stale() -> Failure {
    (
        ExtensionRequestFailure::NotForegroundOwner,
        "Pi settings parent, owner or generation is unavailable".into(),
    )
}

fn parse_object(bytes: &[u8]) -> Result<Map<String, Value>, String> {
    reject_duplicate_json_keys(bytes).map_err(|error| error.to_string())?;
    serde_json::from_slice::<Value>(bytes)
        .map_err(|error| error.to_string())?
        .as_object()
        .cloned()
        .ok_or_else(|| "Pi settings must be a JSON object".into())
}

fn read_object(path: &Path) -> Result<Map<String, Value>, String> {
    match secure_fs::read_regular_file_bounded(path, LIMIT) {
        Ok(bytes) => parse_object(&bytes),
        Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Map::new())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn read(config: &PiSettingsConfig, project_trusted: bool) -> Value {
    let global_path = config.agent_dir.join("settings.json");
    let project_path = config.workspace.join(".pi/settings.json");
    let mut errors = Vec::new();
    let mut scope = |name: &str, path: &Path| match read_object(path) {
        Ok(settings) => settings,
        Err(message) => {
            errors.push(json!({"scope":name,"path":path,"message":message}));
            Map::new()
        }
    };
    let global = scope("global", &global_path);
    let project = if project_trusted {
        scope("project", &project_path)
    } else {
        Map::new()
    };
    json!({
        "global":global,"project":project,"project_trusted":project_trusted,
        "global_path":global_path,"project_path":project_path,"errors":errors,
    })
}

fn builtin_disable_selector(selector: &str) -> bool {
    let Some(name) = selector.strip_prefix("-builtin:") else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_lowercase()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn update(
    config: &PiSettingsConfig,
    requested: &[String],
    cancelled: impl Fn() -> bool,
) -> Result<(), Failure> {
    if requested.len() > 1024 || requested.iter().any(|value| value.len() > 4096) {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            "Pi extension selectors exceed settings bounds".into(),
        ));
    }
    if cancelled() {
        return Err(stale());
    }
    let path = config.agent_dir.join("settings.json");
    let prepared = PreparedMutation::prepare(&path, true, LIMIT)
        .map_err(|error| invalid(error.to_string()))?;
    let mut current = match prepared.original() {
        Some(bytes) => parse_object(bytes).map_err(invalid)?,
        None => Map::new(),
    };
    let selectors: Vec<String> = match current.get("extensions") {
        None => Vec::new(),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| invalid("existing Pi extensions must be an array of strings"))?,
    };
    if cancelled() {
        return Err(stale());
    }
    if requested == selectors.as_slice() {
        return Ok(());
    }
    if requested.len() != selectors.len() + 1
        || !requested.starts_with(&selectors)
        || !builtin_disable_selector(&requested[selectors.len()])
        || selectors.contains(&requested[selectors.len()])
    {
        return Err(invalid(
            "Pi settings update requires the current ordered selectors plus one new builtin-disable selector",
        ));
    }
    current.insert("extensions".into(), json!(requested));
    let bytes = serde_json::to_vec_pretty(&current).expect("settings are JSON");
    if bytes.len() > LIMIT {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            "Pi settings update exceeds 64 KiB".into(),
        ));
    }
    prepared
        .commit_if(&bytes, cancelled)
        .map_err(|error| match error {
            SecureFileError::Cancelled => stale(),
            _ => invalid(error.to_string()),
        })
}

pub(super) fn dispatch(
    state: &ProtocolReadState,
    object: &Map<String, Value>,
    method: &str,
    params: Value,
) -> Result<(), String> {
    let id = parse_child_request_id(object, method)?;
    if read_std_lock(&state.protocol).version != EXTENSION_API_VERSION_0_4
        || state.pi_settings.is_none()
    {
        return reject_typed_child_request(
            state,
            id,
            ExtensionRequestFailure::UnsupportedFeature,
            "Pi settings requires API 0.4 and native-owned settings bindings",
        );
    }
    if let Err(detail) = validate_remote_ui_envelope(object, true) {
        return reject_typed_child_request(
            state,
            id,
            ExtensionRequestFailure::InvalidRequest,
            detail,
        );
    }
    let Some((request, admitted)) =
        admit_host_request::<SettingsRequest>(state, object, method, FEATURE, params)?
    else {
        return Ok(());
    };
    let service = state.pi_settings.as_ref().expect("checked native binding");
    let config = &service.config;
    let validation = (|| {
        if request.cwd != config.workspace || request.agent_dir != config.agent_dir {
            return Err(invalid(
                "Pi settings paths do not match native-owned bindings",
            ));
        }
        if request.project_trusted && !config.project_trusted {
            return Err(invalid(
                "native workspace trust does not permit project settings",
            ));
        }
        if request
            .resource_owner
            .as_ref()
            .is_some_and(|owner| owner != &admitted.owner)
        {
            return Err(stale());
        }
        if method == "pi/settings/read" {
            if request.scope.is_some() || request.extensions.is_some() {
                return Err(invalid("Pi settings read cannot carry an update"));
            }
        } else if request.scope.as_deref() != Some("global") || request.extensions.is_none() {
            return Err(invalid(
                "Pi settings update requires global extension selectors",
            ));
        }
        validate_explicit_request_owner(state, &admitted.owner)?;
        Ok(())
    })();
    if let Err(failure) = validation {
        return refuse_admitted_request(state, &admitted, failure);
    }
    let permit = match state.writer.try_reserve() {
        Ok(permit) => permit,
        Err(error) => {
            settle_child_request(&state.child_requests, &admitted.request_id);
            return Err(format!("Pi settings response writer unavailable: {error}"));
        }
    };
    let completed = (|| {
        let pending = lock_std_mutex(&state.pending);
        let owners = lock_std_mutex(&state.issued_resource_owners);
        let mut children = lock_std_mutex(&state.child_requests);
        let child = children.get(&admitted.request_id).ok_or_else(stale)?;
        let cancelled = || {
            state.closed.load(Ordering::Acquire)
                || state.draining.load(Ordering::Acquire)
                || lock_std_mutex(&state.tombstones).contains(request.parent_request_id)
        };
        let parent_matches = pending
            .get(&request.parent_request_id)
            .is_some_and(|parent| {
                parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                    && parent.resource_owner.as_ref() == Some(&admitted.owner)
            })
            || lock_std_mutex(&service.parents).get(&request.parent_request_id)
                == Some(&admitted.owner);
        if cancelled()
            || !parent_matches
            || !owners.contains(&admitted.owner)
            || child.response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE
            || child.parent_request_id != 0
                && pending.get(&child.parent_request_id).is_none_or(|parent| {
                    parent.terminal.load(Ordering::Acquire) != REQUEST_ACTIVE
                        || parent.resource_owner.as_ref() != Some(&admitted.owner)
                })
        {
            return Err(stale());
        }
        let result = if method == "pi/settings/read" {
            read(config, request.project_trusted)
        } else {
            json!({})
        };
        let mut line = serde_json::to_vec(&json!({
            "jsonrpc":"2.0","id":admitted.request_id,"result":result,
        }))
        .expect("Pi settings response is JSON");
        line.push(b'\n');
        if line.len() > state.max_message_bytes() {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                "Pi settings response exceeds transport limit".into(),
            ));
        }
        if method == "pi/settings/update" {
            update(
                config,
                request.extensions.as_ref().expect("checked update"),
                cancelled,
            )?;
        }
        let response = Arc::clone(&child.response_state);
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
    match completed {
        Ok(()) => Ok(()),
        Err(failure) => refuse_admitted_request(state, &admitted, failure),
    }
}

#[cfg(all(test, unix))]
mod process_tests;
#[cfg(test)]
mod tests;

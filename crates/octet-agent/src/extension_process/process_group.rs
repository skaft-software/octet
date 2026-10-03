//! OS process groups and job objects that contain every process an extension starts, and their teardown.

use super::*;

/// The newest executable-extension API implemented by this octet release.
pub const EXTENSION_API_VERSION: &str = EXTENSION_API_VERSION_0_4;

/// Frozen compatibility version for simple, trusted text extensions.
pub const EXTENSION_API_VERSION_0_1: &str = "0.1";

/// Stateful extension protocol with cancellation, progress, and lifecycle.
pub const EXTENSION_API_VERSION_0_2: &str = "0.2";

/// Schema-generated canonical extension protocol foundation.
pub const EXTENSION_API_VERSION_0_3: &str = api_v03::API_VERSION;

/// Power-parity protocol: the union of every `0.2` and `0.3` capability plus
/// inline components, host-mediated session/model/provider control, themes,
/// host flag projection, and the extended event set. `0.2` and `0.3` manifests
/// validate the same union (no version-adaptation layers); only `0.1` remains
/// restricted.
pub const EXTENSION_API_VERSION_0_4: &str = "0.4";

/// API `0.2` cooperative request cancellation feature.
pub const EXTENSION_FEATURE_REQUEST_CANCELLATION: &str = "request_cancellation";

/// API `0.2` structured/media result feature.
pub const EXTENSION_FEATURE_CONTENT_PARTS: &str = "content_parts";

/// API `0.2` correlated progress feature.
pub const EXTENSION_FEATURE_REQUEST_PROGRESS: &str = "request_progress";

/// API `0.2` bounded replaceable tool-panel decoration feature.
///
/// This remains separate from ordinary status/output progress so API `0.1`
/// and unnegotiated API `0.2` extensions keep their existing wire contract.
pub const EXTENSION_FEATURE_PROGRESS_DECORATION: &str = "progress_decoration";

/// API `0.2` host-ingested artifact feature.
pub const EXTENSION_FEATURE_ARTIFACTS: &str = "artifacts";

/// API `0.2` observational lifecycle feature.
pub const EXTENSION_FEATURE_LIFECYCLE_EVENTS: &str = "lifecycle_events";

/// API `0.2` host-mediated action-intent feature.
pub const EXTENSION_FEATURE_POLICY_INTENTS: &str = "policy_intents";

/// API `0.2` live extension-owned tool catalog feature.
pub const EXTENSION_FEATURE_DYNAMIC_TOOLS: &str = "dynamic_tools";

/// API `0.2` initialization-time command discovery feature.
///
/// Compatibility hosts may not know their complete command set until they load
/// the foreign runtime during initialization. Negotiating this feature lets the
/// initialize response define that generation's fixed command catalog without
/// duplicating those names in the static manifest. It does not permit command
/// mutations after initialization.
pub const EXTENSION_FEATURE_RUNTIME_COMMANDS: &str = "runtime_commands";

/// API `0.2` host-owned child model-session service.
pub const EXTENSION_FEATURE_AGENT_SESSIONS: &str = "agent_sessions";

/// Host-confirmed configured worker routing and bounded discovery.
pub const EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1: &str = "agent_model_selection_v1";

/// API `0.2` first-party delegation telemetry contract.
pub const EXTENSION_FEATURE_DELEGATION_TELEMETRY: &str = "delegation_telemetry_v1";

/// Stable schema label shown by `/extensions status`.
pub const DELEGATION_TELEMETRY_SCHEMA: &str = "octet.delegation.telemetry.v1";

/// API `0.2` single-use host approval capability service.
pub const EXTENSION_FEATURE_APPROVALS: &str = "approvals";

/// API `0.2` owner-scoped host secret lookup service.
pub const EXTENSION_FEATURE_SECRETS: &str = "secrets";

/// API `0.2` bounded semantic UI contribution transport.
///
/// This grants no terminal ownership: extensions can publish only validated
/// snapshots which the frontend projects through its own theme and layout.
pub const EXTENSION_FEATURE_SEMANTIC_UI: &str = "semantic_ui";

/// API `0.2` host-owned editor snapshot and mutation handoff.
pub const EXTENSION_FEATURE_EDITOR_HANDOFF: &str = "editor_handoff";

/// API `0.2` observer-only normalized terminal-input and resize events.
pub const EXTENSION_FEATURE_TERMINAL_INPUT: &str = "terminal_input";

/// API `0.2` bounded host-mediated autocomplete queries.
pub const EXTENSION_FEATURE_AUTOCOMPLETE: &str = "autocomplete";

/// API `0.2` initialization-time semantic tool-renderer discovery.
pub const EXTENSION_FEATURE_DYNAMIC_TOOL_RENDERERS: &str = "dynamic_tool_renderers";

/// API `0.2` host-owned composer snapshot and mutation handoff.
///
/// This grants no terminal ownership: an extension can read and mutate the
/// ordinary host composer, and every operation stays owner-scoped.
pub const EXTENSION_FEATURE_COMPOSER: &str = "composer";

/// API `0.2` runtime shortcut registration and `shortcut/trigger` dispatch.
pub const EXTENSION_FEATURE_SHORTCUTS: &str = "shortcuts";

/// API `0.2` extension-owned durable session entries and session naming.
pub const EXTENSION_FEATURE_SESSION_ENTRIES: &str = "session_entries";

/// API `0.2` bounded host-mediated message injection.
pub const EXTENSION_FEATURE_MESSAGE_INJECTION: &str = "message_injection";

/// API `0.2` coalesced message, compaction, dialog, model, and bash fan-out.
pub const EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2: &str = "lifecycle_events_v2";

/// API `0.2` host-owned active tool selection.
pub const EXTENSION_FEATURE_ACTIVE_TOOLS: &str = "active_tools";

/// API `0.2` foreground terminal handoff request and revocation notification.
///
/// This does not hand the extension a terminal: it asks the frontend that owns
/// the foreground session's tty to cede it for the duration of one grant, and
/// the host keeps the right to revoke that grant with `terminal/grant-lost`.
pub const EXTENSION_FEATURE_TERMINAL_HANDOFF: &str = "terminal_handoff";

/// API `0.2` read-only session-manager and pending-message snapshot requests.
///
/// The host derives every field from the foreground session; the extension
/// supplies only the owner envelope and can never write through this feature.
pub const EXTENSION_FEATURE_SESSION_CONTEXT: &str = "session_context";

/// API `0.2` read-only disclosure of host-owned composed system prompt text.
///
/// This is deliberately a separate negotiated feature because it discloses
/// host-owned prompt text rather than a neutral session snapshot.
pub const EXTENSION_FEATURE_SYSTEM_PROMPT_READ: &str = "system_prompt_read";

/// API `0.2` read-only model view and secret-free model catalog.
///
/// The host publishes what it knows about the selected model and its catalog.
/// Credentials, authorization headers, and provider endpoints are never part of
/// this surface: octet owns provider transport.
pub const EXTENSION_FEATURE_MODEL_CATALOG: &str = "model_catalog";

/// API 0.4 optional, non-authoritative advice before a due cache refresh.
pub const EXTENSION_FEATURE_CACHE_WARMING_DECISION: &str = "cache_warming_decision";

/// API 0.4 host-owned local compaction replacement (vision models only).
pub const EXTENSION_FEATURE_COMPACTION_STRATEGY: &str = "compaction_strategy";

/// Optional API `0.4` request-scoped host tool composition service.
pub const EXTENSION_FEATURE_TOOL_COMPOSITION: &str = "tool_composition_v1";

pub(super) const API_0_2_REQUIRED_FEATURES: &[&str] = &[
    EXTENSION_FEATURE_REQUEST_CANCELLATION,
    EXTENSION_FEATURE_CONTENT_PARTS,
];

pub(super) const API_0_2_OPTIONAL_FEATURES: &[&str] = &[
    EXTENSION_FEATURE_REQUEST_PROGRESS,
    EXTENSION_FEATURE_PROGRESS_DECORATION,
    EXTENSION_FEATURE_ARTIFACTS,
    EXTENSION_FEATURE_LIFECYCLE_EVENTS,
    EXTENSION_FEATURE_POLICY_INTENTS,
    EXTENSION_FEATURE_DYNAMIC_TOOLS,
    EXTENSION_FEATURE_RUNTIME_COMMANDS,
    EXTENSION_FEATURE_SEMANTIC_UI,
    EXTENSION_FEATURE_EDITOR_HANDOFF,
    EXTENSION_FEATURE_TERMINAL_INPUT,
    EXTENSION_FEATURE_AUTOCOMPLETE,
    EXTENSION_FEATURE_DYNAMIC_TOOL_RENDERERS,
    EXTENSION_FEATURE_COMPOSER,
    EXTENSION_FEATURE_SHORTCUTS,
    EXTENSION_FEATURE_SESSION_ENTRIES,
    EXTENSION_FEATURE_MESSAGE_INJECTION,
    EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2,
    EXTENSION_FEATURE_ACTIVE_TOOLS,
    EXTENSION_FEATURE_TERMINAL_HANDOFF,
    EXTENSION_FEATURE_SESSION_CONTEXT,
    EXTENSION_FEATURE_MODEL_CATALOG,
];

pub(super) const MAX_EXTENSION_AGENT_WAIT_MS: u64 = 60_000;

pub(super) const MAX_EXTENSION_SECRET_NAME_BYTES: usize = 64;

// These narrowly reviewed session variables let an explicitly configured
// desktop integration reach the same interactive display/session as its host.
// Values are forwarded only to an API 0.2+ extension that names them in its
// manifest; the extension must separately opt each variable into any child
// process it launches. XAUTHORITY and DBUS_SESSION_BUS_ADDRESS carry access to
// the user's desktop session and must never become ambient defaults.
// SYSTEMROOT and WINDIR are the Windows equivalents of the already-admitted
// USERPROFILE/APPDATA: they locate the system installation and carry no
// credential. A Windows driver child cannot resolve its own runtime without
// them, so admitting them here is what makes a declared Windows-native
// capability installable rather than rejected at manifest validation.
// The Linux desktop-identity names (XDG_CURRENT_DESKTOP, XDG_SESSION_DESKTOP,
// DESKTOP_SESSION, KDE_FULL_SESSION) and compositor IPC names
// (HYPRLAND_INSTANCE_SIGNATURE, SWAYSOCK) are the Linux equivalent: they name
// the running desktop and a compositor socket that already lives under the
// brokered XDG_RUNTIME_DIR. XDG_STATE_HOME locates per-user state like the
// other XDG base directories. None is a credential, and a Linux desktop driver
// cannot select its GNOME, KDE, Hyprland, or Sway route without them.
pub(super) const BROKERED_EXTENSION_ENVIRONMENT: &[&str] = &[
    "SSH_AUTH_SOCK",
    "APPDATA",
    "DBUS_SESSION_BUS_ADDRESS",
    "DESKTOP_SESSION",
    "DISPLAY",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "KDE_FULL_SESSION",
    "LOCALAPPDATA",
    "SWAYSOCK",
    "SYSTEMROOT",
    "USERPROFILE",
    "WAYLAND_DISPLAY",
    "WINDIR",
    "XAUTHORITY",
    "XDG_CONFIG_HOME",
    "XDG_CURRENT_DESKTOP",
    "XDG_DATA_DIRS",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_TYPE",
    "XDG_STATE_HOME",
];

pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

/// The manifest filename inside every extension directory.
pub const EXTENSION_MANIFEST_FILENAME: &str = "extension.toml";

/// Default maximum manifest size (64 KiB).
pub const DEFAULT_EXTENSION_MANIFEST_BYTES: u64 = 64 * 1024;

/// Default maximum size of one JSON protocol message (1 MiB).
pub const DEFAULT_EXTENSION_MESSAGE_BYTES: usize = 1024 * 1024;

pub(super) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) const CONFIRMATION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) const DEFAULT_PENDING_REQUESTS: usize = 64;

pub(super) const DEFAULT_WRITER_QUEUE: usize = 128;

pub(super) const DEFAULT_CANCELLATION_GRACE: Duration = Duration::from_secs(2);

pub(super) const DEFAULT_TOMBSTONE_TTL: Duration = Duration::from_secs(30);

pub(super) const DEFAULT_PROVIDER_STREAM_BUFFER: usize = 32;

pub(super) const DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) const DEFAULT_PROVIDER_STREAM_DEADLINE: Duration = Duration::from_secs(5 * 60);

pub(super) const MAX_TOMBSTONES: usize = 512;

pub(super) const MAX_CHILD_REQUESTS: usize = 128;

pub(super) const MAX_CHILD_WORKERS: usize = 8;

pub(super) const MAX_DYNAMIC_EXTENSION_TOOLS: usize = 256;

pub(super) const MAX_EXTENSION_COMMANDS: usize = 256;

/// Maximum static shortcut declarations accepted from one extension.
pub const MAX_EXTENSION_SHORTCUTS: usize = 64;

/// Maximum UTF-8 bytes in one terminal shortcut spelling.
pub const MAX_EXTENSION_SHORTCUT_KEY_BYTES: usize = 128;

/// Maximum UTF-8 bytes in one shortcut description.
pub const MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES: usize = 4 * 1024;

/// Maximum CLI flags one extension may declare in its manifest.
pub const MAX_EXTENSION_FLAGS: usize = api_v03::MAX_EXTENSION_FLAGS;

pub(super) const MAX_EXTENSION_FLAG_STRING_BYTES: usize = 4 * 1024;

pub(super) const MAX_EXTENSION_FLAG_DESCRIPTION_BYTES: usize = 1024;

pub(super) const DYNAMIC_CATALOG_QUEUE_CAPACITY: usize = 32;

pub(super) const SUPERVISOR_BASE_BACKOFF: Duration = Duration::from_millis(250);

pub(super) const SUPERVISOR_MAX_BACKOFF: Duration = Duration::from_secs(30);

pub(super) const SUPERVISOR_MAX_RESTARTS: u32 = 8;

pub(super) const SUPERVISOR_STABLE_READY: Duration = Duration::from_secs(30);

pub(super) const SUPERVISOR_POLL: Duration = Duration::from_millis(100);

pub(super) static NEXT_EXTENSION_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

/// Maximum distinct extension-originated JSON-RPC IDs in one process
/// generation. IDs are never reusable inside the generation, preventing a
/// delayed frontend answer from targeting a later request with the same ID.
pub const MAX_EXTENSION_CHILD_REQUEST_IDS_PER_GENERATION: usize = 65_536;

/// Maximum queued active-host session lifecycle requests shared by trusted
/// executable extensions. The product drains this queue only at an idle
/// boundary.
pub const MAX_EXTENSION_SESSION_LIFECYCLE_QUEUE: usize = 16;

pub(super) const MAX_LIFECYCLE_REASON_BYTES: usize = 4 * 1024;

/// Maximum UTF-8 bytes in one frontend-minted terminal handoff grant id.
///
/// The grant id is minted by the granting frontend, not by the host, so this is
/// the documented cap that frontend must stay inside: the host never rewrites a
/// grant id it forwards to an extension.
pub const MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES: usize = 128;

/// A declared session hook is a bounded lifecycle finalizer, never a request-path interceptor.
pub(super) const SESSION_HOOK_DEADLINE: Duration = Duration::from_millis(250);

/// Maximum UTF-8 prompt bytes admitted by an API `0.2` input request.
pub const MAX_EXTENSION_INPUT_PROMPT_BYTES: usize = 16 * 1024;

/// Maximum UTF-8 answer bytes returned to an API `0.2` input request.
pub const MAX_EXTENSION_INPUT_VALUE_BYTES: usize = 256 * 1024;

/// Maximum ordered parts admitted from one API `0.2` tool result.
pub const MAX_EXTENSION_RESULT_CONTENT_PARTS: usize = 256;

/// Maximum aggregate referenced media bytes in one API `0.2` tool result.
/// Repeated references count repeatedly because downstream encoders may copy
/// each occurrence.
pub const MAX_EXTENSION_RESULT_MEDIA_BYTES: u64 = 64 * 1024 * 1024;

pub(super) const MAX_SCHEMA_VALIDATION_STEPS: usize = 65_536;

pub(super) const EXTENSION_EVENT_CAPACITY: usize = 128;

pub(super) const MAX_PRESENTATION_UPDATES_PER_SECOND: usize = 32;

// Retain every answered confirmation that can still be buffered for another
// event subscriber. Once this many newer confirmations have been answered, an
// older event has necessarily fallen outside the broadcast channel's window.
pub(super) const ANSWERED_CONFIRMATION_CAPACITY: usize = EXTENSION_EVENT_CAPACITY;

pub(super) const MAX_CONFIRMATION_REQUEST_ID_BYTES: usize = 256;

/// Maximum bytes in one semantic UI key.
pub const MAX_EXTENSION_UI_KEY_BYTES: usize = 128;

/// Maximum bytes in one semantic UI text field or widget line.
pub const MAX_EXTENSION_UI_TEXT_BYTES: usize = 8 * 1024;

/// Maximum widget lines in one semantic UI snapshot.
pub const MAX_EXTENSION_UI_LINES: usize = 32;

/// Maximum custom working-indicator frames in one snapshot.
pub const MAX_EXTENSION_UI_INDICATOR_FRAMES: usize = 16;

/// Maximum persistent keyed status or widget entries from one extension generation.
pub const MAX_EXTENSION_UI_ENTRIES: usize = 64;

/// Maximum editor text bytes admitted through the host-owned handoff.
pub const MAX_EXTENSION_EDITOR_TEXT_BYTES: usize = 256 * 1024;

/// Maximum autocomplete items from one extension query.
pub const MAX_EXTENSION_AUTOCOMPLETE_ITEMS: usize = 32;

/// Maximum bytes in one autocomplete item field.
pub const MAX_EXTENSION_AUTOCOMPLETE_TEXT_BYTES: usize = 1024;

/// Maximum bytes in one normalized observer input payload.
pub const MAX_EXTENSION_TERMINAL_INPUT_BYTES: usize = 256;

/// Maximum composer text bytes admitted by `composer/set`/`composer/insert`.
/// Mirrors [`MAX_EXTENSION_EDITOR_TEXT_BYTES`] (256 KiB).
pub const MAX_EXTENSION_COMPOSER_TEXT_BYTES: usize = MAX_EXTENSION_EDITOR_TEXT_BYTES;

/// Maximum bytes in one runtime-registered shortcut identifier.
/// Mirrors [`MAX_EXTENSION_UI_KEY_BYTES`] (128 bytes).
pub const MAX_EXTENSION_SHORTCUT_ID_BYTES: usize = MAX_EXTENSION_UI_KEY_BYTES;

/// Maximum bytes in one extension-owned session entry type name.
/// Mirrors [`MAX_EXTENSION_UI_KEY_BYTES`] (128 bytes).
pub const MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES: usize = MAX_EXTENSION_UI_KEY_BYTES;

/// Maximum canonical JSON bytes in one session entry payload.
/// Mirrors [`DEFAULT_EXTENSION_MANIFEST_BYTES`] (64 KiB).
pub const MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES: usize = 64 * 1024;

/// Maximum bytes in one session entry label.
/// Mirrors [`MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES`] (4 KiB).
pub const MAX_EXTENSION_SESSION_LABEL_BYTES: usize = MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES;

/// Maximum bytes in one host session name.
/// Mirrors [`MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES`] (4 KiB).
pub const MAX_EXTENSION_SESSION_NAME_BYTES: usize = MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES;

/// Maximum injected message bytes admitted by `session/send_message` and
/// `session/send_user_message`. Mirrors [`MAX_EXTENSION_EDITOR_TEXT_BYTES`].
pub const MAX_EXTENSION_INJECTED_MESSAGE_BYTES: usize = MAX_EXTENSION_EDITOR_TEXT_BYTES;

/// Maximum bytes in one `bash/user` command text.
/// Mirrors [`MAX_EXTENSION_UI_TEXT_BYTES`] (8 KiB).
pub const MAX_EXTENSION_BASH_COMMAND_BYTES: usize = MAX_EXTENSION_UI_TEXT_BYTES;

/// Maximum bounded detail appended to one typed request error message.
/// Mirrors the order of magnitude of [`MAX_CONFIRMATION_REQUEST_ID_BYTES`].
pub const MAX_EXTENSION_REQUEST_ERROR_DETAIL_BYTES: usize = 512;

/// Maximum UTF-8 bytes in one disclosed host system prompt. Large but finite:
/// the host fails the request rather than truncating disclosure silently.
/// Mirrors [`MAX_EXTENSION_EDITOR_TEXT_BYTES`] (256 KiB).
pub const MAX_EXTENSION_SYSTEM_PROMPT_BYTES: usize = MAX_EXTENSION_EDITOR_TEXT_BYTES;

/// Maximum UTF-8 bytes in one context snapshot label (session name, model, or
/// reasoning). Mirrors [`MAX_EXTENSION_SESSION_NAME_BYTES`] (4 KiB).
pub const MAX_EXTENSION_CONTEXT_LABEL_BYTES: usize = MAX_EXTENSION_SESSION_NAME_BYTES;

/// Maximum UTF-8 bytes in one context snapshot path (for example `cwd`).
/// Mirrors [`MAX_EXTENSION_EDITOR_TEXT_BYTES`] (256 KiB).
pub const MAX_EXTENSION_CONTEXT_PATH_BYTES: usize = MAX_EXTENSION_EDITOR_TEXT_BYTES;

/// Maximum active-skill summaries in one session-manager snapshot.
/// Mirrors [`MAX_EXTENSION_AUTOCOMPLETE_ITEMS`].
pub const MAX_EXTENSION_CONTEXT_ACTIVE_SKILLS: usize = 32;

/// Maximum UTF-8 bytes in one active-skill identifier or name.
/// Mirrors [`MAX_EXTENSION_UI_KEY_BYTES`] (128 bytes).
pub const MAX_EXTENSION_CONTEXT_SKILL_FIELD_BYTES: usize = MAX_EXTENSION_UI_KEY_BYTES;

/// Maximum bytes in one coalesced `message/updated` batch. Well under
/// [`DEFAULT_EXTENSION_MESSAGE_BYTES`] and above the
/// `MESSAGE_DELTA_FLUSH_BYTES` boundary which normally flushes first.
pub const MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES: usize = 8 * 1024;

/// Accumulated coalesced bytes which force a `message/updated` flush.
pub(super) const MESSAGE_DELTA_FLUSH_BYTES: usize = 4096;

/// Accumulated coalesced deltas which force a `message/updated` flush.
pub(super) const MESSAGE_DELTA_FLUSH_DELTAS: u64 = 64;

/// Time since the first unflushed delta which forces a flush at the next push
/// or explicit flush. A coalescer never opens a per-delta round trip.
pub(super) const MESSAGE_DELTA_FLUSH_INTERVAL: Duration = Duration::from_millis(50);

pub(super) static HOST_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

pub(super) static HOST_SHUTDOWN_NOTIFY: LazyLock<Notify> = LazyLock::new(Notify::new);

/// Return the non-secret ambient environment permitted for model-controlled
/// subprocesses. Provider credentials, application tokens, dynamic-loader
/// controls, and arbitrary dotenv values are intentionally absent.
pub fn sanitized_subprocess_environment() -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "COLORTERM",
        "NO_COLOR",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "OCTET_PACKAGE_DIR",
    ];
    ALLOWED
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| ((*name).into(), value)))
        .collect()
}

pub(super) fn brokered_extension_environment(
    names: &[String],
) -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
    // The closure keeps `var_os`'s borrow generic enough for the injected
    // lookup signature; passing the function item directly fails to unify.
    brokered_extension_environment_from(names, |name| std::env::var_os(name))
}

pub(super) fn brokered_extension_environment_from(
    names: &[String],
    mut lookup: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> BTreeMap<std::ffi::OsString, std::ffi::OsString> {
    names
        .iter()
        .filter(|name| BROKERED_EXTENSION_ENVIRONMENT.contains(&name.as_str()))
        .filter_map(|name| {
            lookup(name).and_then(|value| {
                if value.is_empty() {
                    None
                } else {
                    Some((std::ffi::OsString::from(name), value))
                }
            })
        })
        .collect()
}

/// Marks the host as shutting down and cancels ordinary extension RPC work.
///
/// The flag is level-triggered so calls which start after the signal cannot
/// miss it. Protocol shutdown requests use a separate path and remain allowed.
pub fn begin_host_shutdown() {
    HOST_SHUTDOWN_REQUESTED.store(true, Ordering::Release);
    HOST_SHUTDOWN_NOTIFY.notify_waiters();
    #[cfg(unix)]
    if let Some(reaper) = LazyLock::force(&PROCESS_REAPER) {
        reaper.unpark();
    }
}

pub(super) async fn host_shutdown_requested() {
    loop {
        let notified = HOST_SHUTDOWN_NOTIFY.notified();
        if HOST_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegisteredProcessKind {
    Bash,
    Extension,
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
pub(super) const PROCESS_IDENTITY_TRACKING_AVAILABLE: bool = true;

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
pub(super) const PROCESS_IDENTITY_TRACKING_AVAILABLE: bool = false;

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProcessIdentity {
    pub(super) pid: i32,
    pub(super) start_time: u128,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(super) struct ProcessSnapshot {
    pub(super) identity: ProcessIdentity,
    pub(super) parent_pid: i32,
    pub(super) process_group_id: i32,
    pub(super) is_zombie: bool,
}

#[cfg(unix)]
impl ProcessSnapshot {
    pub(super) fn is_live(self) -> bool {
        !self.is_zombie
    }
}

#[cfg(unix)]
pub(super) struct DetachedBashSupervision {
    pub(super) deadline: Instant,
    pub(super) cancellation: CancellationToken,
}

#[cfg(unix)]
pub(super) struct RegisteredProcessGroup {
    pub(super) kind: RegisteredProcessKind,
    pub(super) registration_id: u64,
    pub(super) root: Option<ProcessIdentity>,
    pub(super) original_group_active: bool,
    pub(super) descendants: BTreeMap<i32, ProcessIdentity>,
    pub(super) detached_bash: Option<DetachedBashSupervision>,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(super) struct RegisteredProcessScanState {
    pub(super) registration_id: u64,
    pub(super) direct_bash_child_owned: bool,
}

#[cfg(unix)]
pub(super) static REGISTERED_PROCESS_GROUPS: LazyLock<
    StdMutex<BTreeMap<i32, RegisteredProcessGroup>>,
> = LazyLock::new(|| StdMutex::new(BTreeMap::new()));

/// Keep process scan/application order monotonic. Otherwise an older scan can
/// finish last and erase descendants recorded by a newer scan.
#[cfg(unix)]
pub(super) static PROCESS_SNAPSHOT_REFRESH: LazyLock<StdMutex<()>> =
    LazyLock::new(|| StdMutex::new(()));

pub(super) static NEXT_PROCESS_GROUP_REGISTRATION_ID: AtomicU64 = AtomicU64::new(1);

/// A registered Windows job: the kind of process it supervises and a weak
/// handle, so a job that has already been torn down drops out of the registry
/// on the next retention sweep.
#[cfg(windows)]
pub(super) type RegisteredWindowsJob = (RegisteredProcessKind, Weak<WindowsJob>);

/// Live Windows job objects, keyed by the registration id handed back to the
/// owning [`ProcessGroupGuard`]. This is the Windows counterpart of the Unix
/// `REGISTERED_PROCESS_GROUPS` map; see [`WindowsJob::terminate`].
#[cfg(windows)]
pub(super) type WindowsJobRegistry = StdMutex<BTreeMap<u64, RegisteredWindowsJob>>;

#[cfg(windows)]
pub(super) static WINDOWS_PROCESS_JOBS: LazyLock<WindowsJobRegistry> =
    LazyLock::new(|| StdMutex::new(BTreeMap::new()));

#[cfg(windows)]
pub(super) struct WindowsJob {
    pub(super) handle: OwnedHandle,
    pub(super) terminated: AtomicBool,
}

#[cfg(windows)]
impl WindowsJob {
    pub(super) fn create() -> std::io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned handle for this process.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut limits = unsafe { std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle().cast(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of_mut!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            terminated: AtomicBool::new(false),
        })
    }

    pub(super) fn terminate(&self) {
        if !self.terminated.swap(true, Ordering::AcqRel) {
            // The Job Object owns the exact process tree assigned by the
            // suspended launch handshake; it cannot target an unrelated PID.
            unsafe {
                let _ = TerminateJobObject(self.handle.as_raw_handle().cast(), 1);
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn register_windows_job(kind: RegisteredProcessKind, job: &Arc<WindowsJob>) -> u64 {
    let registration_id = NEXT_PROCESS_GROUP_REGISTRATION_ID.fetch_add(1, Ordering::Relaxed);
    let mut registered = lock_std_mutex(&WINDOWS_PROCESS_JOBS);
    registered.retain(|_, (_, job)| job.strong_count() != 0);
    registered.insert(registration_id, (kind, Arc::downgrade(job)));
    registration_id
}

#[cfg(windows)]
pub(super) fn unregister_windows_job(registration_id: u64) {
    lock_std_mutex(&WINDOWS_PROCESS_JOBS).remove(&registration_id);
}

#[cfg(windows)]
pub(super) fn windows_jobs(kind: Option<RegisteredProcessKind>) -> Vec<Arc<WindowsJob>> {
    let mut registered = lock_std_mutex(&WINDOWS_PROCESS_JOBS);
    registered.retain(|_, (_, job)| job.strong_count() != 0);
    registered
        .values()
        .filter(|(registered_kind, _)| kind.is_none_or(|wanted| wanted == *registered_kind))
        .filter_map(|(_, job)| job.upgrade())
        .collect()
}

#[cfg(unix)]
pub(super) const PROCESS_REAPER_POLL: Duration = Duration::from_millis(25);

/// One process-wide supervisor records PID/start-time identities for descendants
/// and reaps successful `bash` groups with residual background work. A standard
/// thread keeps the cleanup boundary alive during async-runtime teardown.
#[cfg(unix)]
pub(super) static PROCESS_REAPER: LazyLock<Option<std::thread::Thread>> = LazyLock::new(|| {
    std::thread::Builder::new()
        .name("octet-process-reaper".into())
        .spawn(process_reaper_loop)
        .ok()
        .map(|handle| handle.thread().clone())
});

#[cfg(unix)]
pub(super) fn valid_process_group_id(process_group_id: u64) -> Option<i32> {
    i32::try_from(process_group_id)
        .ok()
        .filter(|process_group_id| *process_group_id > 0)
}

#[cfg(not(windows))]
pub(super) fn register_process_group(process_group_id: u64, kind: RegisteredProcessKind) -> u64 {
    let registration_id = NEXT_PROCESS_GROUP_REGISTRATION_ID.fetch_add(1, Ordering::Relaxed);
    #[cfg(unix)]
    if let Some(process_group_id) = valid_process_group_id(process_group_id) {
        let root = process_identity(process_group_id);
        lock_std_mutex(&REGISTERED_PROCESS_GROUPS).insert(
            process_group_id,
            RegisteredProcessGroup {
                kind,
                registration_id,
                root,
                original_group_active: root.is_some(),
                descendants: BTreeMap::new(),
                detached_bash: None,
            },
        );
        if let Some(reaper) = LazyLock::force(&PROCESS_REAPER) {
            reaper.unpark();
        }
    }
    #[cfg(not(unix))]
    let _ = (process_group_id, kind);
    registration_id
}

#[cfg(unix)]
pub(super) fn remove_registered_process_group(
    process_group_id: i32,
    registration_id: u64,
) -> Option<RegisteredProcessGroup> {
    let mut registered = lock_std_mutex(&REGISTERED_PROCESS_GROUPS);
    if registered
        .get(&process_group_id)
        .is_some_and(|entry| entry.registration_id == registration_id)
    {
        registered.remove(&process_group_id)
    } else {
        None
    }
}

#[cfg(not(windows))]
pub(super) fn unregister_process_group(process_group_id: u64, registration_id: u64) -> bool {
    #[cfg(unix)]
    if let Some(process_group_id) = valid_process_group_id(process_group_id) {
        return remove_registered_process_group(process_group_id, registration_id).is_some();
    }
    #[cfg(not(unix))]
    let _ = (process_group_id, registration_id);
    false
}

/// RAII ownership for a child placed in its own process group.
///
/// Dropping an armed guard force-terminates the whole group. Call
/// [`ProcessGroupGuard::disarm`] only after the direct child has been waited
/// and all captured output pipes have closed.
pub struct ProcessGroupGuard {
    pub(super) process_group_id: AtomicU64,
    pub(super) registration_id: u64,
    #[cfg(windows)]
    pub(super) job: Option<Arc<WindowsJob>>,
    #[cfg(windows)]
    pub(super) disarmed: AtomicBool,
}

#[derive(Clone)]
pub(super) struct ProcessTerminationHandle {
    #[cfg(not(windows))]
    pub(super) process_group_id: u64,
    #[cfg(not(windows))]
    pub(super) registration_id: u64,
    #[cfg(windows)]
    pub(super) job: Arc<WindowsJob>,
}

impl ProcessTerminationHandle {
    pub(super) fn terminate(self) {
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(not(windows))]
        terminate_registered_process_group(
            self.process_group_id,
            self.registration_id,
            libc_sigkill(),
        );
    }
}

impl ProcessGroupGuard {
    /// Registers a shell or built-in `bash` child process group.
    #[cfg(not(windows))]
    pub fn bash(pid: Option<u32>) -> Self {
        Self::new(pid.map(u64::from).unwrap_or(0), RegisteredProcessKind::Bash)
    }

    #[cfg(not(windows))]
    pub(super) fn extension(process_group_id: u64) -> Self {
        Self::new(process_group_id, RegisteredProcessKind::Extension)
    }

    #[cfg(not(windows))]
    pub(super) fn new(process_group_id: u64, kind: RegisteredProcessKind) -> Self {
        let registration_id = register_process_group(process_group_id, kind);
        Self {
            process_group_id: AtomicU64::new(process_group_id),
            registration_id,
        }
    }

    #[cfg(windows)]
    pub(super) fn from_windows(
        process_id: u32,
        kind: RegisteredProcessKind,
        job: Arc<WindowsJob>,
        registration_id: u64,
    ) -> Self {
        let _ = kind;
        Self {
            process_group_id: AtomicU64::new(u64::from(process_id)),
            registration_id,
            job: Some(job),
            disarmed: AtomicBool::new(false),
        }
    }

    pub(super) fn termination_handle(&self) -> ProcessTerminationHandle {
        #[cfg(windows)]
        {
            ProcessTerminationHandle {
                job: self
                    .job
                    .as_ref()
                    .expect("Windows process guard always owns a Job Object")
                    .clone(),
            }
        }
        #[cfg(not(windows))]
        {
            ProcessTerminationHandle {
                process_group_id: self.process_group_id.load(Ordering::Acquire),
                registration_id: self.registration_id,
            }
        }
    }

    /// Immediately force-terminates the owned process group.
    pub fn terminate_now(&self) {
        #[cfg(windows)]
        {
            if !self.disarmed.load(Ordering::Acquire) {
                if let Some(job) = &self.job {
                    job.terminate();
                }
            }
            self.process_group_id.store(0, Ordering::Release);
        }
        #[cfg(not(windows))]
        {
            let process_group_id = self.process_group_id.swap(0, Ordering::AcqRel);
            terminate_registered_process_group(
                process_group_id,
                self.registration_id,
                libc_sigkill(),
            );
        }
    }

    /// Releases the group after its child and output pipes have fully settled.
    pub fn disarm(&self) {
        #[cfg(windows)]
        {
            if !self.disarmed.swap(true, Ordering::AcqRel) {
                // A successful Bash root may have started background work. The
                // bounded Windows route does not leave that work outside the
                // host-owned Job Object; close it only after all pipes settled.
                if let Some(job) = &self.job {
                    job.terminate();
                }
                unregister_windows_job(self.registration_id);
            }
            self.process_group_id.store(0, Ordering::Release);
        }
        #[cfg(not(windows))]
        {
            let process_group_id = self.process_group_id.swap(0, Ordering::AcqRel);
            unregister_process_group(process_group_id, self.registration_id);
        }
    }

    #[cfg(unix)]
    pub(super) fn has_root_identity(&self, root: ProcessIdentity) -> bool {
        let process_group_id = self.process_group_id.load(Ordering::Acquire);
        let Some(process_group_id) = valid_process_group_id(process_group_id) else {
            return false;
        };
        lock_std_mutex(&REGISTERED_PROCESS_GROUPS)
            .get(&process_group_id)
            .is_some_and(|entry| {
                entry.registration_id == self.registration_id
                    && entry.kind == RegisteredProcessKind::Bash
                    && entry.root == Some(root)
            })
    }

    #[cfg(unix)]
    pub(super) fn refresh_bash_descendants(&self) {
        refresh_registered_descendants();
    }

    /// Transfers a successfully reaped direct `bash` child to the centralized
    /// descendant supervisor. The registry entry remains live until the group
    /// disappears naturally, the run is cancelled, the original execution
    /// deadline expires, or host shutdown begins.
    pub fn supervise_bash_descendants(self, lifetime: Duration, cancellation: CancellationToken) {
        #[cfg(unix)]
        {
            let process_group_id = self.process_group_id.load(Ordering::Acquire);
            let Some(process_group_id_i32) = valid_process_group_id(process_group_id) else {
                self.disarm();
                return;
            };
            // A whole-process-table scan can race the shell's final exit and
            // miss a background child that has already inherited its group.
            // Retry the bounded handoff while the freshly created group still
            // exists; stable PID/start-time identities remain mandatory.
            let mut descendants_found = false;
            for _ in 0..3 {
                refresh_registered_descendants();
                let handoff_is_bound = if PROCESS_IDENTITY_TRACKING_AVAILABLE {
                    registered_process_has_live_identity(process_group_id_i32, self.registration_id)
                } else {
                    registered_process_is_alive(process_group_id_i32, self.registration_id)
                };
                if handoff_is_bound {
                    descendants_found = true;
                    break;
                }
                if !process_group_is_alive(process_group_id_i32) {
                    break;
                }
                std::thread::yield_now();
            }
            if !descendants_found {
                self.disarm();
                return;
            }
            if lifetime.is_zero()
                || cancellation.is_cancelled()
                || HOST_SHUTDOWN_REQUESTED.load(Ordering::Acquire)
            {
                self.terminate_now();
                return;
            }
            let now = Instant::now();
            let Some(deadline) = now.checked_add(lifetime) else {
                self.terminate_now();
                return;
            };
            let Some(reaper) = LazyLock::force(&PROCESS_REAPER) else {
                self.terminate_now();
                return;
            };

            let transferred = {
                let mut registered = lock_std_mutex(&REGISTERED_PROCESS_GROUPS);
                let Some(entry) = registered.get_mut(&process_group_id_i32) else {
                    return;
                };
                if entry.registration_id != self.registration_id
                    || entry.kind != RegisteredProcessKind::Bash
                {
                    return;
                }
                entry.detached_bash = Some(DetachedBashSupervision {
                    deadline,
                    cancellation,
                });
                // Transfer ownership while holding the registry lock. The
                // reaper cannot observe the detached state before Drop becomes
                // harmless, avoiding a stale post-reap signal after PGID reuse.
                self.process_group_id.store(0, Ordering::Release);
                true
            };
            if transferred {
                reaper.unpark();
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (lifetime, cancellation);
            self.disarm();
        }
    }
}

#[cfg(windows)]
/// Prepares and registers a Windows process in an exact, private Job Object.
///
/// The child is created suspended so assignment succeeds before any extension
/// or shell code can run. Dropping the resulting guard terminates the owned
/// job tree.
pub struct WindowsProcessLaunch {
    pub(super) job: Arc<WindowsJob>,
    pub(super) kind: RegisteredProcessKind,
    pub(super) registration_id: u64,
}

#[cfg(windows)]
impl WindowsProcessLaunch {
    /// Prepare a Bash-compatible child for Job Object supervision.
    pub fn bash(command: &mut Command) -> std::io::Result<Self> {
        Self::prepare(command, RegisteredProcessKind::Bash)
    }

    /// Prepare an executable extension child for Job Object supervision.
    pub fn extension(command: &mut Command) -> std::io::Result<Self> {
        Self::prepare(command, RegisteredProcessKind::Extension)
    }

    pub(super) fn prepare(
        command: &mut Command,
        kind: RegisteredProcessKind,
    ) -> std::io::Result<Self> {
        let job = Arc::new(WindowsJob::create()?);
        let registration_id = register_windows_job(kind, &job);
        // No application code runs until the process is assigned to the Job
        // Object. A failed assignment therefore fails closed rather than
        // falling back to direct-child cleanup.
        command.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_SUSPENDED
                | windows_sys::Win32::System::Threading::CREATE_NO_WINDOW,
        );
        Ok(Self {
            job,
            kind,
            registration_id,
        })
    }

    /// Assign the suspended child to the Job Object and resume it.
    pub fn register(self, child: &Child) -> std::io::Result<ProcessGroupGuard> {
        let process_id = child.id().ok_or_else(|| {
            unregister_windows_job(self.registration_id);
            std::io::Error::other("spawned Windows process did not expose a process ID")
        })?;
        let process = unsafe {
            OpenProcess(
                PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_SUSPEND_RESUME,
                0,
                process_id,
            )
        };
        if process.is_null() {
            unregister_windows_job(self.registration_id);
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: OpenProcess returned a handle owned by this scope.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let assigned = unsafe {
            AssignProcessToJobObject(
                self.job.handle.as_raw_handle().cast(),
                process.as_raw_handle().cast(),
            )
        };
        if assigned == 0 {
            unregister_windows_job(self.registration_id);
            return Err(std::io::Error::last_os_error());
        }
        let status = unsafe { NtResumeProcess(process.as_raw_handle().cast()) };
        if status < 0 {
            self.job.terminate();
            unregister_windows_job(self.registration_id);
            return Err(std::io::Error::other(format!(
                "failed to resume Windows process: NTSTATUS {status:#x}"
            )));
        }
        Ok(ProcessGroupGuard::from_windows(
            process_id,
            self.kind,
            self.job,
            self.registration_id,
        ))
    }
}

#[cfg(unix)]
pub(super) const BASH_LAUNCH_READY: u8 = b'L';

#[cfg(unix)]
pub(super) const BASH_HANDOFF_READY: u8 = b'H';

#[cfg(unix)]
pub(super) const BASH_HANDOFF_ACK: u8 = b'A';

/// POSIX `sh` implementations commonly support only single-digit redirections.
#[cfg(unix)]
pub(super) const BASH_CONTROL_FD: RawFd = 9;

/// Gates a shell source until its process identity is registered, then adds a
/// completion handshake before its leader exits.
///
/// The handshake keeps the direct shell alive while the process registry scans
/// its descendants. That captures a child which has left the original process
/// group with `setsid` before macOS can lose its parent relationship.
#[cfg(unix)]
pub struct BashProcessLaunch {
    pub(super) control: UnixStream,
    pub(super) child_control: Option<OwnedFd>,
    pub(super) control_fd_reservation: Option<OwnedFd>,
    pub(super) source: String,
}

#[cfg(unix)]
impl BashProcessLaunch {
    /// Configures `command` to start `source` only after registration.
    pub fn prepare(command: &mut Command, source: &str) -> std::io::Result<Self> {
        let (parent, child) = StdUnixStream::pair()?;
        let parent = move_control_fd_out_of_stdio(parent)?;
        let child = move_control_fd_out_of_stdio(child)?;
        parent.set_nonblocking(true)?;
        let control = UnixStream::from_std(parent)?;
        let parent_fd = control.as_raw_fd();
        set_close_on_exec(parent_fd)?;
        let child_fd = child.as_raw_fd();
        set_close_on_exec(child_fd)?;
        // SAFETY: `child` owns this newly created descriptor. Keeping it open
        // in the parent until spawn makes it available to the post-fork child;
        // `register` closes the parent copy before it releases the source gate.
        let child_control = unsafe { OwnedFd::from_raw_fd(child.into_raw_fd()) };
        let control_fd_reservation = reserve_bash_control_fd(child_control.as_raw_fd())?;
        // SAFETY: the closure runs after fork and only closes the parent's
        // endpoint before clearing CLOEXEC on the child's endpoint. The shell
        // itself blocks on the generated source gate after it has exec'd, so
        // `Command::spawn` can still return its PID to the parent.
        unsafe {
            command
                .pre_exec(move || prepare_bash_process_exec(parent_fd, child_fd, BASH_CONTROL_FD));
        }
        Ok(Self {
            control,
            child_control: Some(child_control),
            control_fd_reservation,
            source: format!(
                "IFS= read -r __octet_bash_launch <&{BASH_CONTROL_FD}\n[ \"$__octet_bash_launch\" = 'L' ] || exit 127\n__octet_bash_handoff() {{\n    __octet_bash_status=$?\n    trap - 0\n    printf 'H' >&{BASH_CONTROL_FD}\n    IFS= read -r __octet_bash_handoff_ack <&{BASH_CONTROL_FD}\n    exit \"$__octet_bash_status\"\n}}\ntrap '__octet_bash_handoff' 0\n{source}\n"
            ),
        })
    }

    /// Returns the shell source with registry gates and handoff trap appended.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Registers the gated direct child, releases its source gate, and returns
    /// the guard and handoff receiver used while waiting for the child.
    pub async fn register(
        mut self,
        pid: Option<u32>,
    ) -> std::io::Result<(ProcessGroupGuard, BashProcessHandoff)> {
        drop(self.child_control.take());
        drop(self.control_fd_reservation.take());
        let pid =
            pid.ok_or_else(|| std::io::Error::other("spawned shell did not expose a process ID"))?;
        let root = if PROCESS_IDENTITY_TRACKING_AVAILABLE {
            let pid_i32 = i32::try_from(pid).map_err(|_| {
                std::io::Error::other("spawned shell process ID does not fit in i32")
            })?;
            // The shell is blocked at the first generated source instruction,
            // so a missing identity means we cannot safely establish ownership.
            // Dropping `self` makes the source gate fail before any user command
            // can run.
            Some(process_identity(pid_i32).ok_or_else(|| {
                std::io::Error::other("could not establish the spawned shell process identity")
            })?)
        } else {
            // Other Unix targets retain the existing process-group fallback.
            None
        };
        let guard = ProcessGroupGuard::bash(Some(pid));
        if let Some(root) = root {
            // Registration takes a second identity snapshot. Requiring it to
            // match prevents a PID reuse between the pre-release check and
            // registry bind from granting this execution authority over a
            // replacement process.
            if !guard.has_root_identity(root) {
                return Err(std::io::Error::other(
                    "could not register the spawned shell process identity",
                ));
            }
        }
        guard.refresh_bash_descendants();
        self.control.write_all(&[BASH_LAUNCH_READY, b'\n']).await?;
        Ok((
            guard,
            BashProcessHandoff {
                control: self.control,
            },
        ))
    }
}

/// Receives the shell-completion signal that keeps the leader alive while its
/// descendants are captured by the process registry.
#[cfg(unix)]
pub struct BashProcessHandoff {
    pub(super) control: UnixStream,
}

#[cfg(unix)]
impl BashProcessHandoff {
    pub(super) async fn capture_descendants(mut self) {
        let mut ready = [0_u8; 1];
        if self.control.read_exact(&mut ready).await.is_err() || ready[0] != BASH_HANDOFF_READY {
            return;
        }
        refresh_registered_descendants();
        let _ = self.control.write_all(&[BASH_HANDOFF_ACK, b'\n']).await;
    }
}

/// Waits for a direct shell child while servicing its registry handoff.
///
/// If the shell reaches its completion trap, it cannot exit until this function
/// captures descendants and acknowledges it. A shell that exits or `exec`s
/// before the trap retains the existing post-exit scan fallback.
#[cfg(unix)]
pub async fn wait_for_bash_process(
    child: &mut Child,
    handoff: BashProcessHandoff,
) -> std::io::Result<ExitStatus> {
    let handoff = handoff.capture_descendants();
    tokio::pin!(handoff);
    tokio::select! {
        biased;
        _ = &mut handoff => child.wait().await,
        status = child.wait() => status,
    }
}

#[cfg(unix)]
pub(super) fn move_control_fd_out_of_stdio(
    stream: StdUnixStream,
) -> std::io::Result<StdUnixStream> {
    if stream.as_raw_fd() > libc::STDERR_FILENO {
        return Ok(stream);
    }
    // SAFETY: `stream` owns the source descriptor. F_DUPFD creates an
    // independent descriptor at or above 3, so child stdio setup cannot
    // overwrite the control channel before the shell executes.
    let duplicate = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD, 3) };
    if duplicate == -1 {
        return Err(std::io::Error::last_os_error());
    }
    drop(stream);
    // SAFETY: `duplicate` was returned by fcntl above and is now owned here.
    Ok(unsafe { StdUnixStream::from_raw_fd(duplicate) })
}

#[cfg(unix)]
pub(super) fn reserve_bash_control_fd(child_fd: RawFd) -> std::io::Result<Option<OwnedFd>> {
    // Keep FD 9 occupied across `Command::spawn` so its internal exec-status
    // pipe cannot be allocated there and then overwritten by the child-side
    // `dup2`. If FD 9 was already occupied, it already provides that reserve.
    let duplicate = unsafe { libc::fcntl(child_fd, libc::F_DUPFD, BASH_CONTROL_FD) };
    if duplicate == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fcntl returned this owned duplicate. Its Drop closes it on both
    // the success and error paths below.
    let duplicate = unsafe { OwnedFd::from_raw_fd(duplicate) };
    if duplicate.as_raw_fd() != BASH_CONTROL_FD {
        return Ok(None);
    }
    set_close_on_exec(duplicate.as_raw_fd())?;
    Ok(Some(duplicate))
}

#[cfg(unix)]
pub(super) fn set_close_on_exec(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: `fd` is owned by the newly created socket pair and remains valid
    // for this process while the `UnixStream` value is alive.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the descriptor and flags were read above. Keeping the parent
    // copy close-on-exec prevents unrelated concurrent spawns from inheriting
    // the handoff endpoint.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn prepare_bash_process_exec(
    parent_fd: RawFd,
    child_fd: RawFd,
    control_fd: RawFd,
) -> std::io::Result<()> {
    // SAFETY: this runs in the post-fork child and uses only async-signal-safe
    // descriptor operations. The raw parent descriptor is closed only in this
    // child; the parent keeps its Tokio stream. `dup2` reserves a descriptor
    // accepted by POSIX `sh` implementations before the shell executes.
    unsafe {
        let _ = libc::close(parent_fd);
        if child_fd != control_fd && libc::dup2(child_fd, control_fd) == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let flags = libc::fcntl(control_fd, libc::F_GETFD);
        if flags == -1 || libc::fcntl(control_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate_now();
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn linux_process_snapshot(pid: i32) -> Option<ProcessSnapshot> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat.get(stat.rfind(") ")?.saturating_add(2)..)?;
    let fields = fields.split_ascii_whitespace().collect::<Vec<_>>();
    let is_zombie = fields.first()?.as_bytes() == b"Z";
    let parent_pid = fields.get(1)?.parse().ok()?;
    let process_group_id = fields.get(2)?.parse().ok()?;
    let start_time = fields.get(19)?.parse::<u128>().ok()?;
    Some(ProcessSnapshot {
        identity: ProcessIdentity { pid, start_time },
        parent_pid,
        process_group_id,
        is_zombie,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn process_snapshots() -> Vec<ProcessSnapshot> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(linux_process_snapshot)
        .filter(|snapshot| snapshot.is_live())
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn apple_process_snapshot(pid: i32) -> Option<ProcessSnapshot> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let size_i32 = i32::try_from(size).ok()?;
    // SAFETY: `info` points to exactly `size_i32` writable bytes and
    // PROC_PIDTBSDINFO initializes the complete proc_bsdinfo on success.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size_i32,
        )
    };
    if read != size_i32 {
        // Darwin can stop serving PROC_PIDTBSDINFO for an exited, unreaped
        // process while `kill(pid, 0)` still succeeds. Treat the absent
        // snapshot as no identity; falling back to `kill` would revive a zombie.
        return None;
    }
    // SAFETY: the exact-size success check above proves initialization.
    let info = unsafe { info.assume_init() };
    if i32::try_from(info.pbi_pid).ok()? != pid {
        return None;
    }
    let parent_pid = i32::try_from(info.pbi_ppid).ok()?;
    let process_group_id = i32::try_from(info.pbi_pgid).ok()?;
    let start_time = u128::from(info.pbi_start_tvsec)
        .saturating_mul(1_000_000)
        .saturating_add(u128::from(info.pbi_start_tvusec));
    Some(ProcessSnapshot {
        identity: ProcessIdentity { pid, start_time },
        parent_pid,
        process_group_id,
        is_zombie: info.pbi_status == libc::SZOMB,
    })
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn apple_related_pids(process_id: i32, children: bool) -> Vec<i32> {
    // SAFETY: null/zero queries ask libproc only for the required PID count.
    let count = unsafe {
        if children {
            libc::proc_listchildpids(process_id, std::ptr::null_mut(), 0)
        } else {
            libc::proc_listpgrppids(process_id, std::ptr::null_mut(), 0)
        }
    };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let capacity = count.saturating_add(16);
    let mut pids = vec![0_i32; capacity];
    let Ok(bytes) = i32::try_from(capacity.saturating_mul(std::mem::size_of::<i32>())) else {
        return Vec::new();
    };
    // SAFETY: `pids` exposes `bytes` writable bytes and libproc returns no more
    // PID entries than fit in the supplied buffer.
    let listed = unsafe {
        if children {
            libc::proc_listchildpids(process_id, pids.as_mut_ptr().cast(), bytes)
        } else {
            libc::proc_listpgrppids(process_id, pids.as_mut_ptr().cast(), bytes)
        }
    };
    let Ok(listed) = usize::try_from(listed) else {
        return Vec::new();
    };
    pids.truncate(listed.min(pids.len()));
    pids.retain(|pid| *pid > 0);
    pids
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn process_snapshots() -> Vec<ProcessSnapshot> {
    let (mut pending, active_groups) = {
        let registered = lock_std_mutex(&REGISTERED_PROCESS_GROUPS);
        let mut pending = BTreeSet::new();
        let mut active_groups = Vec::new();
        for (process_group_id, entry) in registered.iter() {
            pending.extend(entry.root.into_iter().map(|identity| identity.pid));
            pending.extend(entry.descendants.keys().copied());
            if entry.original_group_active {
                active_groups.push(*process_group_id);
            }
        }
        (pending, active_groups)
    };
    for process_group_id in active_groups {
        pending.extend(apple_related_pids(process_group_id, false));
    }

    let mut discovered = BTreeSet::new();
    let mut snapshots = Vec::new();
    while let Some(pid) = pending.pop_first() {
        if !discovered.insert(pid) {
            continue;
        }
        let Some(snapshot) = apple_process_snapshot(pid) else {
            continue;
        };
        if !snapshot.is_live() {
            continue;
        }
        pending.extend(apple_related_pids(pid, true));
        snapshots.push(snapshot);
    }
    snapshots
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
pub(super) fn process_snapshots() -> Vec<ProcessSnapshot> {
    Vec::new()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn process_snapshot(pid: i32) -> Option<ProcessSnapshot> {
    linux_process_snapshot(pid)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn process_snapshot(pid: i32) -> Option<ProcessSnapshot> {
    apple_process_snapshot(pid)
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
pub(super) fn process_snapshot(pid: i32) -> Option<ProcessSnapshot> {
    process_snapshots()
        .into_iter()
        .find(|snapshot| snapshot.identity.pid == pid)
}

#[cfg(unix)]
pub(super) fn process_identity(pid: i32) -> Option<ProcessIdentity> {
    process_snapshot(pid)
        .filter(|snapshot| snapshot.is_live())
        .map(|snapshot| snapshot.identity)
}

#[cfg(unix)]
pub(super) fn process_identity_is_alive(identity: ProcessIdentity) -> bool {
    process_identity(identity.pid) == Some(identity)
}

#[cfg(unix)]
pub(super) fn refresh_registered_descendants() {
    let _refresh_guard = lock_std_mutex(&PROCESS_SNAPSHOT_REFRESH);
    // Process discovery runs without the registry lock. Record which
    // registrations and directly-owned state the scan can describe so a group
    // inserted, replaced, or handed off while the scan is in flight is never
    // invalidated by stale observations.
    let (registration_states_at_snapshot_start, known_identities) = {
        let registered = lock_std_mutex(&REGISTERED_PROCESS_GROUPS);
        let registration_states = registered
            .iter()
            .map(|(process_group_id, entry)| {
                (
                    *process_group_id,
                    RegisteredProcessScanState {
                        registration_id: entry.registration_id,
                        direct_bash_child_owned: entry.kind == RegisteredProcessKind::Bash
                            && entry.detached_bash.is_none(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let identities = registered
            .values()
            .flat_map(|entry| {
                entry
                    .root
                    .into_iter()
                    .chain(entry.descendants.values().copied())
            })
            .collect::<Vec<_>>();
        (registration_states, identities)
    };
    let snapshots = process_snapshots();
    if snapshots.is_empty() {
        return;
    }
    let mut snapshots_by_pid = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.identity.pid, snapshot))
        .collect::<BTreeMap<_, _>>();
    // Whole-system enumeration is not atomic and platform APIs may briefly
    // omit a live process. Re-read every missing identity directly before
    // treating it as dead and severing ownership of its descendants.
    for identity in known_identities {
        if snapshots_by_pid.contains_key(&identity.pid) {
            continue;
        }
        let Some(snapshot) = process_snapshot(identity.pid).filter(|snapshot| snapshot.is_live())
        else {
            continue;
        };
        snapshots_by_pid.insert(identity.pid, snapshot);
    }
    let mut registered = lock_std_mutex(&REGISTERED_PROCESS_GROUPS);
    apply_process_snapshots(
        &mut registered,
        &registration_states_at_snapshot_start,
        &snapshots_by_pid,
    );
}

#[cfg(unix)]
pub(super) fn apply_process_snapshots(
    registered: &mut BTreeMap<i32, RegisteredProcessGroup>,
    registration_states_at_snapshot_start: &BTreeMap<i32, RegisteredProcessScanState>,
    snapshots_by_pid: &BTreeMap<i32, ProcessSnapshot>,
) {
    for (process_group_id, entry) in registered.iter_mut() {
        let Some(scan_state) = registration_states_at_snapshot_start.get(process_group_id) else {
            continue;
        };
        if scan_state.registration_id != entry.registration_id {
            continue;
        }
        entry.descendants.retain(|pid, identity| {
            snapshots_by_pid
                .get(pid)
                .is_some_and(|snapshot| snapshot.identity == *identity)
        });

        let root_alive = entry.root.and_then(|identity| {
            snapshots_by_pid
                .get(&identity.pid)
                .filter(|snapshot| snapshot.identity == identity)
                .copied()
        });
        let mut owned = entry.descendants.clone();
        if let Some(root) = root_alive {
            owned.insert(root.identity.pid, root.identity);
        }
        let group_has_member = snapshots_by_pid
            .values()
            .any(|snapshot| snapshot.process_group_id == *process_group_id);
        // A scan may straddle the direct child's exit and its descendant's
        // creation. Keep the freshly owned group bound until Bash hands it to
        // detached supervision; the handoff performs fresh scans below.
        let group_is_bound =
            entry.original_group_active && (group_has_member || scan_state.direct_bash_child_owned);

        loop {
            let mut changed = false;
            for snapshot in snapshots_by_pid.values() {
                if owned.contains_key(&snapshot.identity.pid) {
                    continue;
                }
                let child_of_owned = owned.contains_key(&snapshot.parent_pid);
                let original_group_member =
                    group_is_bound && snapshot.process_group_id == *process_group_id;
                if child_of_owned || original_group_member {
                    owned.insert(snapshot.identity.pid, snapshot.identity);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        if let Some(root) = entry.root {
            owned.remove(&root.pid);
        }
        entry.original_group_active = group_is_bound;
        entry.descendants = owned;
    }
}

#[cfg(unix)]
#[derive(Clone)]
pub(super) struct RegisteredProcessTargets {
    pub(super) process_group_id: i32,
    pub(super) root: Option<ProcessIdentity>,
    pub(super) descendants: Vec<ProcessIdentity>,
}

#[cfg(unix)]
impl RegisteredProcessTargets {
    pub(super) fn identities(&self) -> impl Iterator<Item = ProcessIdentity> + '_ {
        self.root
            .into_iter()
            .chain(self.descendants.iter().copied())
    }
}

#[cfg(unix)]
pub(super) fn targets_from_registered(
    process_group_id: i32,
    registered: &RegisteredProcessGroup,
) -> RegisteredProcessTargets {
    RegisteredProcessTargets {
        process_group_id,
        root: registered.root,
        descendants: registered.descendants.values().copied().collect(),
    }
}

#[cfg(unix)]
pub(super) fn registered_process_keys(kind: Option<RegisteredProcessKind>) -> Vec<(i32, u64)> {
    lock_std_mutex(&REGISTERED_PROCESS_GROUPS)
        .iter()
        .filter_map(|(process_group_id, registered)| {
            kind.is_none_or(|kind| kind == registered.kind)
                .then_some((*process_group_id, registered.registration_id))
        })
        .collect()
}

#[cfg(unix)]
pub(super) fn registered_targets(
    process_group_id: i32,
    registration_id: u64,
) -> Option<RegisteredProcessTargets> {
    lock_std_mutex(&REGISTERED_PROCESS_GROUPS)
        .get(&process_group_id)
        .filter(|registered| registered.registration_id == registration_id)
        .map(|registered| targets_from_registered(process_group_id, registered))
}

#[cfg(unix)]
pub(super) fn detached_bash_process_groups() -> Vec<(i32, u64, Instant, CancellationToken)> {
    lock_std_mutex(&REGISTERED_PROCESS_GROUPS)
        .iter()
        .filter_map(|(process_group_id, registered)| {
            registered.detached_bash.as_ref().map(|supervision| {
                (
                    *process_group_id,
                    registered.registration_id,
                    supervision.deadline,
                    supervision.cancellation.clone(),
                )
            })
        })
        .collect()
}

#[cfg(unix)]
pub(super) fn signal_identity(identity: ProcessIdentity, signal: i32) {
    if process_identity(identity.pid) != Some(identity) {
        return;
    }
    // SAFETY: the immediately preceding start-time check binds this PID to the
    // process recorded while it was a descendant of octet's registered child.
    unsafe {
        let _ = libc::kill(identity.pid, signal);
    }
}

#[cfg(unix)]
pub(super) fn signal_registered_targets(targets: &RegisteredProcessTargets, signal: i32) {
    let group_is_bound = targets.identities().any(|identity| {
        process_snapshot(identity.pid).is_some_and(|snapshot| {
            snapshot.is_live()
                && snapshot.identity == identity
                && snapshot.process_group_id == targets.process_group_id
        })
    });
    if group_is_bound
        || (!PROCESS_IDENTITY_TRACKING_AVAILABLE
            && process_group_is_alive(targets.process_group_id))
    {
        // SAFETY: supported platforms require a matching PID/start-time
        // identity in the group, so its ID cannot name an unrelated group.
        // Other Unix targets retain the legacy best-effort group cleanup.
        unsafe {
            let _ = libc::kill(-targets.process_group_id, signal);
        }
    }
    for identity in targets.identities() {
        signal_identity(identity, signal);
    }
}

#[cfg(unix)]
pub(super) fn registered_process_has_live_identity(
    process_group_id: i32,
    registration_id: u64,
) -> bool {
    registered_targets(process_group_id, registration_id)
        .is_some_and(|targets| targets.identities().any(process_identity_is_alive))
}

#[cfg(unix)]
pub(super) fn registered_process_is_alive(process_group_id: i32, registration_id: u64) -> bool {
    let Some(targets) = registered_targets(process_group_id, registration_id) else {
        return false;
    };
    let mut identities = targets.identities().peekable();
    if identities.peek().is_none() {
        return !PROCESS_IDENTITY_TRACKING_AVAILABLE && process_group_is_alive(process_group_id);
    }
    identities.any(process_identity_is_alive)
}

#[cfg(not(windows))]
pub(super) fn libc_sigkill() -> i32 {
    #[cfg(unix)]
    {
        libc::SIGKILL
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(not(windows))]
pub(super) fn terminate_registered_process_group(
    process_group_id: u64,
    registration_id: u64,
    signal: i32,
) {
    #[cfg(unix)]
    {
        refresh_registered_descendants();
        let Some(process_group_id) = valid_process_group_id(process_group_id) else {
            return;
        };
        if let Some(registered) = remove_registered_process_group(process_group_id, registration_id)
        {
            let targets = targets_from_registered(process_group_id, &registered);
            signal_registered_targets(&targets, signal);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (registration_id, signal);
        kill_process_group(process_group_id);
    }
}

#[cfg(unix)]
pub(super) fn process_reaper_loop() {
    loop {
        refresh_registered_descendants();
        let has_registered = !lock_std_mutex(&REGISTERED_PROCESS_GROUPS).is_empty();
        if !has_registered {
            std::thread::park();
            continue;
        }

        let now = Instant::now();
        let host_shutdown = HOST_SHUTDOWN_REQUESTED.load(Ordering::Acquire);
        let mut next_poll = PROCESS_REAPER_POLL;
        for (process_group_id, registration_id, deadline, cancellation) in
            detached_bash_process_groups()
        {
            let alive = registered_process_is_alive(process_group_id, registration_id);
            let terminate = host_shutdown || cancellation.is_cancelled() || now >= deadline;
            if !alive {
                unregister_process_group(process_group_id as u64, registration_id);
                continue;
            }
            if terminate {
                terminate_registered_process_group(
                    process_group_id as u64,
                    registration_id,
                    libc::SIGKILL,
                );
                continue;
            }
            next_poll = next_poll.min(deadline.saturating_duration_since(now));
        }
        if next_poll.is_zero() {
            std::thread::yield_now();
        } else {
            std::thread::park_timeout(next_poll);
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) fn process_group_registered_for_test(process_group_id: i32) -> bool {
    lock_std_mutex(&REGISTERED_PROCESS_GROUPS).contains_key(&process_group_id)
}

/// Test diagnostic that treats tracked zombies as exited, unlike `kill(pid, 0)`.
#[cfg(all(test, unix))]
pub(crate) fn process_is_live_for_test(pid: i32) -> bool {
    if PROCESS_IDENTITY_TRACKING_AVAILABLE {
        process_identity(pid).is_some()
    } else {
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(unix)]
pub(super) fn process_group_is_alive(process_group_id: i32) -> bool {
    // Signal zero performs existence/permission checking without changing the
    // target. EPERM still means that the group exists.
    let result = unsafe { libc::kill(-process_group_id, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
pub(super) fn signal_registered_processes(keys: &[(i32, u64)], signal: i32) {
    refresh_registered_descendants();
    for (process_group_id, registration_id) in keys {
        if let Some(targets) = registered_targets(*process_group_id, *registration_id) {
            signal_registered_targets(&targets, signal);
        }
    }
}

/// Gracefully terminates registered shell/`bash` process trees, then force-kills
/// and waits for survivors, all within the supplied total timeout.
pub async fn terminate_bash_process_groups(timeout: Duration) {
    #[cfg(unix)]
    {
        let process_keys = registered_process_keys(Some(RegisteredProcessKind::Bash));
        if process_keys.is_empty() {
            return;
        }
        let started = Instant::now();
        let graceful_deadline = started + timeout / 2;
        let final_deadline = started + timeout;
        signal_registered_processes(&process_keys, libc::SIGTERM);

        let mut survivors = process_keys;
        while Instant::now() < graceful_deadline {
            refresh_registered_descendants();
            survivors.retain(|(process_group_id, registration_id)| {
                registered_process_is_alive(*process_group_id, *registration_id)
            });
            if survivors.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        signal_registered_processes(&survivors, libc::SIGKILL);
        while Instant::now() < final_deadline {
            refresh_registered_descendants();
            survivors.retain(|(process_group_id, registration_id)| {
                registered_process_is_alive(*process_group_id, *registration_id)
            });
            if survivors.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    #[cfg(windows)]
    {
        let _ = timeout;
        for job in windows_jobs(Some(RegisteredProcessKind::Bash)) {
            job.terminate();
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = timeout;
}

/// Force-kills every registered shell, `bash`, and extension process tree.
///
/// This is the last-resort watchdog path after coordinated cleanup times out.
pub fn force_kill_registered_process_groups() {
    #[cfg(unix)]
    {
        let process_keys = registered_process_keys(None);
        signal_registered_processes(&process_keys, libc::SIGKILL);
    }
    #[cfg(windows)]
    for job in windows_jobs(None) {
        job.terminate();
    }
}

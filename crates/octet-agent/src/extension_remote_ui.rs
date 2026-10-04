//! Optional API `0.4` cached surfaces rendered and input-routed by the host.
//!
//! This is a language-neutral transport, not terminal ownership or an extension
//! runtime. Frames contain printable UTF-8 and a small, validated SGR subset;
//! the frontend owns layout, exclusivity, and the active session fence.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::extension_process::{
    ExtensionEditorCheckpoint, ExtensionRequestFailure, ExtensionRequestId, ExtensionResourceOwner,
};

/// Optional feature offered only when a remote UI frontend is bound.
pub const EXTENSION_FEATURE_REMOTE_UI: &str = "remote_ui";
/// Maximum ASCII bytes in one extension-local fullscreen surface identifier.
pub const MAX_EXTENSION_REMOTE_UI_SURFACE_ID_BYTES: usize = 64;
/// Maximum UTF-8 bytes in one plain fullscreen title.
pub const MAX_EXTENSION_REMOTE_UI_TITLE_BYTES: usize = 128;
/// Maximum lines in one complete fullscreen frame.
pub const MAX_EXTENSION_REMOTE_UI_LINES: usize = 256;
/// Maximum UTF-8 bytes, including SGR, in one frame line.
pub const MAX_EXTENSION_REMOTE_UI_LINE_BYTES: usize = 16 * 1024;
/// Maximum aggregate UTF-8 bytes, including SGR, in one frame.
pub const MAX_EXTENSION_REMOTE_UI_FRAME_BYTES: usize = 512 * 1024;
/// Maximum UTF-8 bytes in one normalized host key spelling.
pub const MAX_EXTENSION_REMOTE_UI_KEY_BYTES: usize = 128;
/// Maximum UTF-8 bytes in one host closure reason.
pub const MAX_EXTENSION_REMOTE_UI_REASON_BYTES: usize = 4 * 1024;
/// Maximum reserved or open surfaces in one process generation.
pub const MAX_EXTENSION_REMOTE_UI_SURFACES: usize = 16;
/// Maximum numeric SGR parameter bytes in one escape sequence.
pub const MAX_EXTENSION_REMOTE_UI_SGR_BYTES: usize = 128;
/// Largest exactly representable JSON frame revision.
pub const MAX_EXTENSION_REMOTE_UI_REVISION: u64 = 9_007_199_254_740_991;

type ValidationResult = Result<(), (ExtensionRequestFailure, String)>;

/// Host-owned layout slot requested by an extension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRemoteUiPlacement {
    /// Exclusive fullscreen surface with focused input.
    #[default]
    Fullscreen,
    /// Host header rectangle.
    Header,
    /// Host footer rectangle.
    Footer,
    /// Host rectangle immediately above the composer.
    AboveEditor,
    /// Host rectangle immediately below the composer.
    BelowEditor,
    /// Host-owned editor replacement rectangle.
    Editor,
}

/// Extension request to open a host-rendered cached surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiOpenRequest {
    /// Original host request supplying the authoritative owner while active.
    pub parent_request_id: u64,
    /// Previously issued owner for a caller that outlived its original request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Extension-local identifier, restricted to ASCII letters, digits, `_`, `-`, and `.`.
    pub surface_id: String,
    /// Plain terminal-safe surface title.
    pub title: String,
    /// Host-owned layout slot; omission preserves the fullscreen contract.
    #[serde(default)]
    pub placement: ExtensionRemoteUiPlacement,
    /// Request host-owned mouse reporting while a fullscreen lease has focus.
    #[serde(default)]
    pub mouse_capture: bool,
}

/// Extension request to close a host-rendered fullscreen surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiCloseRequest {
    /// Original host request supplying the authoritative owner while active.
    pub parent_request_id: u64,
    /// Previously issued owner for a caller that outlived its original request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Extension-local fullscreen surface identifier.
    pub surface_id: String,
}

/// One admitted fullscreen operation awaiting the owning frontend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionRemoteUiOperation {
    /// Open the single host-owned fullscreen input/rendering surface.
    Open {
        /// Extension-local fullscreen surface identifier.
        surface_id: String,
        /// Plain terminal-safe surface title.
        title: String,
        /// Host-owned layout slot.
        #[serde(default)]
        placement: ExtensionRemoteUiPlacement,
        /// Request host-owned mouse reporting for this fullscreen lease.
        #[serde(default)]
        mouse_capture: bool,
    },
    /// Close the matching fullscreen surface.
    Close {
        /// Extension-local fullscreen surface identifier.
        surface_id: String,
    },
}

impl ExtensionRemoteUiOperation {
    /// Validates identifiers and titles without admitting terminal controls.
    pub fn validate(&self) -> ValidationResult {
        match self {
            Self::Open {
                surface_id,
                title,
                placement,
                mouse_capture,
            } => {
                if *mouse_capture && *placement != ExtensionRemoteUiPlacement::Fullscreen {
                    return Err(invalid(
                        "remote UI mouse capture requires fullscreen placement",
                    ));
                }
                validate_surface_id(surface_id)?;
                validate_text(
                    "remote UI title",
                    title,
                    MAX_EXTENSION_REMOTE_UI_TITLE_BYTES,
                    false,
                )
            }
            Self::Close { surface_id } => validate_surface_id(surface_id),
        }
    }
}

/// Dimensions and optional editor fence returned after the host admits a surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiOpenResult {
    /// Available host terminal columns.
    pub columns: u16,
    /// Available host terminal rows.
    pub rows: u16,
    /// Host-issued mount identity, present only for editor placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_mount_id: Option<String>,
}

impl ExtensionRemoteUiOpenResult {
    /// Validates the nonzero host geometry.
    pub fn validate(&self) -> ValidationResult {
        validate_dimensions(self.columns, self.rows)?;
        if let Some(mount_id) = &self.editor_mount_id {
            validate_surface_id(mount_id)?;
        }
        Ok(())
    }
}

/// Empty acknowledgement returned after the host closes the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiCloseResult {}

/// Complete extension-to-host `ui/frame` notification payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiFrameNotification {
    /// Exact owner triple previously issued by this host process generation.
    pub resource_owner: ExtensionResourceOwner,
    /// Extension-local fullscreen surface identifier.
    pub surface_id: String,
    /// Extension-owned frame revision.
    pub revision: u64,
    /// Terminal width this frame was rendered for; stale geometry is not drawn.
    pub columns: u16,
    /// Terminal height this frame was rendered for; stale geometry is not drawn.
    pub rows: u16,
    /// Complete bounded replacement lines; never a delta or a terminal program.
    pub lines: Vec<String>,
}

impl ExtensionRemoteUiFrameNotification {
    /// Validates complete frame bounds and the narrowly admitted SGR grammar.
    /// Owner authority must additionally be checked against the issuing process.
    pub fn validate(&self) -> ValidationResult {
        validate_surface_id(&self.surface_id)?;
        validate_dimensions(self.columns, self.rows)?;
        if self.revision > MAX_EXTENSION_REMOTE_UI_REVISION {
            return Err(bounds(
                "remote UI frame revision exceeds the portable integer limit",
            ));
        }
        if self.lines.len() > MAX_EXTENSION_REMOTE_UI_LINES {
            return Err(bounds("remote UI frame has too many lines"));
        }
        let mut bytes = 0;
        for line in &self.lines {
            if line.len() > MAX_EXTENSION_REMOTE_UI_LINE_BYTES {
                return Err(bounds("remote UI frame line exceeds its byte limit"));
            }
            bytes += line.len();
            if bytes > MAX_EXTENSION_REMOTE_UI_FRAME_BYTES {
                return Err(bounds("remote UI frame exceeds its aggregate byte limit"));
            }
            validate_remote_ui_line(line)
                .map_err(|detail| (ExtensionRequestFailure::InvalidRequest, detail))?;
        }
        Ok(())
    }
}

/// Latest validated frame retained outside the broadcast event queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionRemoteUiFrame {
    /// Issuing process generation, for the frontend's final stale-frame check.
    pub generation: u64,
    /// Host-issued owner triple, for the frontend's active-session check.
    pub resource_owner: ExtensionResourceOwner,
    /// Extension-local fullscreen surface identifier.
    pub surface_id: String,
    /// Extension-owned frame revision.
    pub revision: u64,
    /// Terminal width this frame was rendered for.
    pub columns: u16,
    /// Terminal height this frame was rendered for.
    pub rows: u16,
    /// Complete bounded replacement lines with validated printable text and SGR.
    pub lines: Vec<String>,
}

/// Kind of one host-normalized fullscreen key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRemoteUiKeyKind {
    /// Initial key press.
    Press,
    /// Repeated held-key press, when supported by the terminal.
    Repeat,
    /// Key release, when supported by the terminal.
    Release,
}

/// Modifier held during one host-normalized fullscreen key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRemoteUiKeyModifier {
    /// Shift modifier.
    Shift,
    /// Alt/Option modifier.
    Alt,
    /// Control modifier.
    Control,
    /// Super/Command modifier.
    Super,
}

/// Host-issued input clock for one custom-editor mount. Render clocks are separate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiEditorInput {
    /// Current host-issued editor mount identity, not an extension-local surface ID.
    pub mount_id: String,
    /// Monotonic input revision issued by this mount; starts at one.
    pub input_revision: u64,
}

impl ExtensionRemoteUiEditorInput {
    /// Validates the bounded host input fence, not its live authority.
    pub fn validate(&self) -> ValidationResult {
        validate_surface_id(&self.mount_id)?;
        if self.input_revision == 0 || self.input_revision > MAX_EXTENSION_REMOTE_UI_REVISION {
            return Err(bounds(
                "editor input revision is outside the portable nonzero range",
            ));
        }
        Ok(())
    }
}

/// Host-to-extension `ui/key` notification payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiKey {
    /// Matching fullscreen surface identifier.
    pub surface_id: String,
    /// Normalized key name or printable character, never raw terminal escapes.
    pub key: String,
    /// Press, repeat, or release semantics supplied by the host input backend.
    pub kind: ExtensionRemoteUiKeyKind,
    /// Duplicate-free held modifiers.
    pub modifiers: Vec<ExtensionRemoteUiKeyModifier>,
    /// Editor-only mount/input fence. Non-editor keys retain their existing shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_input: Option<ExtensionRemoteUiEditorInput>,
}

impl ExtensionRemoteUiKey {
    /// Validates the bounded normalized key payload.
    pub fn validate(&self) -> ValidationResult {
        validate_surface_id(&self.surface_id)?;
        if let Some(editor_input) = &self.editor_input {
            editor_input.validate()?;
        }
        validate_text(
            "remote UI key",
            &self.key,
            MAX_EXTENSION_REMOTE_UI_KEY_BYTES,
            false,
        )?;
        if self.modifiers.len() > 4 {
            return Err(bounds("remote UI key has too many modifiers"));
        }
        for (index, modifier) in self.modifiers.iter().enumerate() {
            if self.modifiers[..index].contains(modifier) {
                return Err(invalid("remote UI key modifiers contain duplicates"));
            }
        }
        Ok(())
    }
}

/// Host-normalized mouse event kind; never a raw terminal escape sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRemoteUiMouseKind {
    /// Initial button press.
    Press,
    /// Button release.
    Release,
    /// Motion while a button is held.
    Drag,
    /// Motion without a held button.
    Move,
    /// Wheel scrolling; positive delta means down.
    Wheel,
}

/// Host-normalized mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRemoteUiMouseButton {
    /// Left button.
    Left,
    /// Middle button.
    Middle,
    /// Right button.
    Right,
    /// No button, including wheel and free motion events.
    None,
}

/// Host-to-extension `ui/mouse` notification for an admitted mouse lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiMouse {
    /// Matching fullscreen surface identifier.
    pub surface_id: String,
    /// Normalized mouse event kind.
    pub kind: ExtensionRemoteUiMouseKind,
    /// Normalized button identity.
    pub button: ExtensionRemoteUiMouseButton,
    /// Zero-based terminal column.
    pub x: u16,
    /// Zero-based terminal row.
    pub y: u16,
    /// Duplicate-free held modifiers.
    pub modifiers: Vec<ExtensionRemoteUiKeyModifier>,
    /// Signed wheel distance; positive is down, zero for ordinary motion.
    #[serde(default)]
    pub wheel_delta: i16,
}

impl ExtensionRemoteUiMouse {
    /// Validates bounded identity and modifiers; the host checks live geometry.
    pub fn validate(&self) -> ValidationResult {
        validate_surface_id(&self.surface_id)?;
        if self.modifiers.len() > 4 {
            return Err(bounds("remote UI mouse has too many modifiers"));
        }
        for (index, modifier) in self.modifiers.iter().enumerate() {
            if self.modifiers[..index].contains(modifier) {
                return Err(invalid("remote UI mouse modifiers contain duplicates"));
            }
        }
        Ok(())
    }
}

/// Host-to-extension `ui/resize` notification payload for a fullscreen surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiResize {
    /// Matching fullscreen surface identifier.
    pub surface_id: String,
    /// Current host terminal columns.
    pub columns: u16,
    /// Current host terminal rows.
    pub rows: u16,
}

impl ExtensionRemoteUiResize {
    /// Validates the fullscreen surface identifier and nonzero geometry.
    pub fn validate(&self) -> ValidationResult {
        validate_surface_id(&self.surface_id)?;
        validate_dimensions(self.columns, self.rows)
    }
}

/// Host-to-extension `ui/closed` notification payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRemoteUiClosed {
    /// Fullscreen surface whose host lease ended.
    pub surface_id: String,
    /// Bounded plain host reason for closing the surface.
    pub reason: String,
}

impl ExtensionRemoteUiClosed {
    /// Validates the surface identifier and plain closure reason.
    pub fn validate(&self) -> ValidationResult {
        validate_surface_id(&self.surface_id)?;
        validate_text(
            "remote UI close reason",
            &self.reason,
            MAX_EXTENSION_REMOTE_UI_REASON_BYTES,
            true,
        )
    }
}

fn invalid(detail: &str) -> (ExtensionRequestFailure, String) {
    (ExtensionRequestFailure::InvalidRequest, detail.to_owned())
}

fn bounds(detail: &str) -> (ExtensionRequestFailure, String) {
    (ExtensionRequestFailure::BoundsExceeded, detail.to_owned())
}

fn validate_dimensions(columns: u16, rows: u16) -> ValidationResult {
    if columns == 0 || rows == 0 {
        return Err(invalid("remote UI dimensions must be nonzero"));
    }
    Ok(())
}

pub(crate) fn validate_surface_id(value: &str) -> ValidationResult {
    if value.len() > MAX_EXTENSION_REMOTE_UI_SURFACE_ID_BYTES {
        return Err(bounds("remote UI surface id exceeds its byte limit"));
    }
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(invalid("remote UI surface id must be nonempty safe ASCII"));
    }
    Ok(())
}

fn validate_text(label: &str, text: &str, limit: usize, allow_empty: bool) -> ValidationResult {
    if text.len() > limit {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            format!("{label} exceeds its byte limit"),
        ));
    }
    if !allow_empty && text.is_empty() {
        return Err((
            ExtensionRequestFailure::InvalidRequest,
            format!("{label} must not be empty"),
        ));
    }
    validate_printable(text).map_err(|()| {
        (
            ExtensionRequestFailure::InvalidRequest,
            format!("{label} contains terminal controls"),
        )
    })
}

fn validate_printable(text: &str) -> Result<(), ()> {
    if text
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        Err(())
    } else {
        Ok(())
    }
}

/// Validates printable UTF-8 plus numeric SGR style/color sequences only.
/// OSC, cursor movement, erase commands, C0/C1 controls, and line breaks are
/// never accepted. Extended colors use only `38/48;5;n` or `38/48;2;r;g;b`
/// with every color component checked as a byte; colon extensions are refused.
pub fn validate_remote_ui_line(line: &str) -> Result<(), String> {
    let mut remaining = line;
    while let Some(escape) = remaining.find('\u{1b}') {
        validate_printable(&remaining[..escape])
            .map_err(|()| "remote UI frame line contains terminal controls".to_owned())?;
        let sequence = remaining[escape..]
            .strip_prefix("\u{1b}[")
            .ok_or_else(|| "remote UI frame line contains a non-SGR escape".to_owned())?;
        let end = sequence
            .find('m')
            .ok_or_else(|| "remote UI frame line contains an unterminated SGR".to_owned())?;
        validate_sgr(&sequence[..end])?;
        remaining = &sequence[end + 1..];
    }
    validate_printable(remaining)
        .map_err(|()| "remote UI frame line contains terminal controls".to_owned())
}

fn validate_sgr(parameters: &str) -> Result<(), String> {
    let invalid = || "remote UI frame line contains an unsupported SGR".to_owned();
    if parameters.len() > MAX_EXTENSION_REMOTE_UI_SGR_BYTES {
        return Err("remote UI frame line contains an oversized SGR".to_owned());
    }
    let mut values = parameters.split(';');
    while let Some(value) = values.next() {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        let value = value.parse::<u16>().map_err(|_| invalid())?;
        match value {
            0..=9 | 21..=37 | 39..=47 | 49 | 90..=97 | 100..=107 => {}
            38 | 48 => {
                let mode = values.next().ok_or_else(invalid)?;
                let components = match mode {
                    "5" => 1,
                    "2" => 3,
                    _ => return Err(invalid()),
                };
                for _ in 0..components {
                    let component = values.next().ok_or_else(invalid)?;
                    if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit())
                    {
                        return Err(invalid());
                    }
                    component.parse::<u8>().map_err(|_| invalid())?;
                }
            }
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

/// One bounded latest-wins cache shared by the reader and current connection.
/// Surface identity includes the complete host-issued owner, not just its name.
pub(crate) struct RemoteUiMailbox {
    wake: Option<Arc<Notify>>,
    surfaces: Mutex<HashMap<(ExtensionResourceOwner, String), RemoteUiSurface>>,
}

struct RemoteUiSurface {
    mouse_capture: bool,
    opening: Option<ExtensionRequestId>,
    parent_request_id: Option<u64>,
    geometry: Option<ExtensionRemoteUiOpenResult>,
    revision: Option<u64>,
    frame: Option<ExtensionRemoteUiFrame>,
}

/// The child table owns this reservation. Dropping an unresolved open, including
/// cancellation and refusal, releases its slot without admitting a surface.
pub(crate) struct RemoteUiChildRequest {
    mailbox: Arc<RemoteUiMailbox>,
    request_id: ExtensionRequestId,
    owner: ExtensionResourceOwner,
    operation: ExtensionRemoteUiOperation,
}

pub(crate) struct RemoteUiResponse {
    mailbox: Arc<RemoteUiMailbox>,
    request_id: ExtensionRequestId,
    owner: ExtensionResourceOwner,
    surface_id: String,
    geometry: Option<ExtensionRemoteUiOpenResult>,
}

impl RemoteUiMailbox {
    pub(crate) fn new(wake: Option<Arc<Notify>>) -> Self {
        Self {
            wake,
            surfaces: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn is_bound(&self) -> bool {
        self.wake.is_some()
    }

    pub(crate) fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake.notify_one();
        }
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        request_id: ExtensionRequestId,
        owner: ExtensionResourceOwner,
        operation: ExtensionRemoteUiOperation,
        parent_request_id: Option<u64>,
    ) -> Result<RemoteUiChildRequest, (ExtensionRequestFailure, String)> {
        operation.validate()?;
        let mut surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match &operation {
            ExtensionRemoteUiOperation::Open {
                surface_id,
                mouse_capture,
                ..
            } => {
                let key = (owner.clone(), surface_id.clone());
                if surfaces.contains_key(&key) {
                    return Err(invalid("remote UI surface is already open or opening"));
                }
                if surfaces.len() >= MAX_EXTENSION_REMOTE_UI_SURFACES {
                    return Err(bounds("remote UI surface limit exceeded"));
                }
                surfaces.insert(
                    key,
                    RemoteUiSurface {
                        mouse_capture: *mouse_capture,
                        opening: Some(request_id.clone()),
                        parent_request_id,
                        geometry: None,
                        revision: None,
                        frame: None,
                    },
                );
            }
            ExtensionRemoteUiOperation::Close { surface_id } => {
                if !surfaces
                    .get(&(owner.clone(), surface_id.clone()))
                    .is_some_and(|surface| surface.geometry.is_some())
                {
                    return Err(invalid("remote UI surface is not open for this owner"));
                }
            }
        }
        Ok(RemoteUiChildRequest {
            mailbox: Arc::clone(self),
            request_id,
            owner,
            operation,
        })
    }

    pub(crate) fn accept(
        &self,
        frame: ExtensionRemoteUiFrameNotification,
        generation: u64,
    ) -> ValidationResult {
        let mut surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let surface = surfaces
            .get_mut(&(frame.resource_owner.clone(), frame.surface_id.clone()))
            .ok_or_else(|| invalid("remote UI frame names an unknown surface or owner"))?;
        let geometry = surface
            .geometry
            .as_ref()
            .ok_or_else(|| invalid("remote UI frame arrived before open admission"))?;
        if geometry.columns != frame.columns || geometry.rows != frame.rows {
            return Err(invalid("remote UI frame geometry is stale"));
        }
        if surface
            .revision
            .is_some_and(|revision| frame.revision <= revision)
        {
            return Err(invalid("remote UI frame revision is stale"));
        }
        surface.revision = Some(frame.revision);
        surface.frame = Some(ExtensionRemoteUiFrame {
            generation,
            resource_owner: frame.resource_owner,
            surface_id: frame.surface_id,
            revision: frame.revision,
            columns: frame.columns,
            rows: frame.rows,
            lines: frame.lines,
        });
        drop(surfaces);
        self.wake();
        Ok(())
    }

    pub(crate) fn take_frames(&self) -> Vec<ExtensionRemoteUiFrame> {
        self.surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values_mut()
            .filter_map(|surface| surface.frame.take())
            .collect()
    }

    pub(crate) fn contains(&self, owner: &ExtensionResourceOwner, surface_id: &str) -> bool {
        self.surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&(owner.clone(), surface_id.to_owned()))
    }

    pub(crate) fn surface_ids(&self) -> Vec<String> {
        let mut ids = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    pub(crate) fn owners(&self) -> Vec<ExtensionResourceOwner> {
        let surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut owners = Vec::new();
        for (owner, _) in surfaces.keys() {
            if !owners.contains(owner) {
                owners.push(owner.clone());
            }
        }
        owners
    }

    // Host notifications do not carry an extension-supplied owner. A reused id
    // across owners must be ambiguous rather than delivering input to either.
    pub(crate) fn host_owner(
        &self,
        surface_id: &str,
    ) -> Result<ExtensionResourceOwner, (ExtensionRequestFailure, String)> {
        let surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut owners = surfaces.iter().filter_map(|((owner, id), surface)| {
            (id == surface_id && surface.geometry.is_some()).then_some(owner)
        });
        let owner = owners
            .next()
            .ok_or_else(|| invalid("remote UI surface is not open"))?;
        if owners.next().is_some() {
            return Err(invalid("remote UI surface id is ambiguous across owners"));
        }
        Ok(owner.clone())
    }

    /// Holds the existing retirement disposition through a synchronous editor
    /// mutation and its reserved writer admission. The closure must not reenter
    /// this mailbox or acquire the parent/child maps (lock order is the reverse).
    pub(crate) fn with_editor_checkpoint(
        &self,
        owner: &ExtensionResourceOwner,
        checkpoint: &ExtensionEditorCheckpoint,
        commit: impl FnOnce() -> ValidationResult,
    ) -> ValidationResult {
        let surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let geometry = surfaces
            .get(&(owner.clone(), checkpoint.surface_id.clone()))
            .and_then(|surface| surface.geometry.as_ref())
            .ok_or_else(|| {
                invalid("editor checkpoint belongs to a retired or unadmitted surface")
            })?;
        if geometry.editor_mount_id.as_deref() != Some(checkpoint.mount_id.as_str()) {
            return Err(invalid(
                "editor checkpoint mount is retired or not an editor",
            ));
        }
        let result = commit();
        drop(surfaces);
        result
    }

    pub(crate) fn validate_mouse(
        &self,
        owner: &ExtensionResourceOwner,
        mouse: &ExtensionRemoteUiMouse,
    ) -> ValidationResult {
        let surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let surface = surfaces
            .get(&(owner.clone(), mouse.surface_id.clone()))
            .ok_or_else(|| invalid("remote UI mouse surface is not open"))?;
        let geometry = surface
            .geometry
            .as_ref()
            .ok_or_else(|| invalid("remote UI mouse surface is not admitted"))?;
        if !surface.mouse_capture {
            return Err(invalid("remote UI mouse capture was not requested"));
        }
        if mouse.x >= geometry.columns || mouse.y >= geometry.rows {
            return Err(invalid("remote UI mouse coordinates exceed live geometry"));
        }
        Ok(())
    }

    pub(crate) fn resize(&self, owner: &ExtensionResourceOwner, resize: &ExtensionRemoteUiResize) {
        let mut surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(surface) = surfaces.get_mut(&(owner.clone(), resize.surface_id.clone())) {
            surface.geometry = Some(ExtensionRemoteUiOpenResult {
                columns: resize.columns,
                rows: resize.rows,
                editor_mount_id: surface
                    .geometry
                    .as_ref()
                    .and_then(|geometry| geometry.editor_mount_id.clone()),
            });
            surface.frame = None;
        }
    }

    pub(crate) fn close(&self, owner: &ExtensionResourceOwner, surface_id: &str) {
        self.surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(owner.clone(), surface_id.to_owned()));
        self.wake();
    }

    pub(crate) fn settle_parent(&self, parent_request_id: u64, cancelled: bool) {
        let mut surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        surfaces.retain(|_, surface| {
            if surface.parent_request_id != Some(parent_request_id) {
                return true;
            }
            if cancelled {
                return false;
            }
            surface.parent_request_id = None;
            true
        });
        drop(surfaces);
        if cancelled {
            self.wake();
        }
    }

    pub(crate) fn discard_owner(&self, owner: &ExtensionResourceOwner) {
        self.surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|(candidate, _), _| candidate != owner);
        self.wake();
    }

    pub(crate) fn clear(&self) {
        self.surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.wake();
    }
}

impl RemoteUiChildRequest {
    pub(crate) fn prepare_response(
        &self,
        result: Option<&serde_json::Value>,
    ) -> Result<Option<RemoteUiResponse>, String> {
        let Some(result) = result else {
            return Ok(None);
        };
        let (surface_id, geometry) = match &self.operation {
            ExtensionRemoteUiOperation::Open {
                surface_id,
                placement,
                ..
            } => {
                let geometry: ExtensionRemoteUiOpenResult = serde_json::from_value(result.clone())
                    .map_err(|error| format!("invalid remote UI open response: {error}"))?;
                geometry.validate().map_err(|(_, detail)| detail)?;
                if geometry.editor_mount_id.is_some()
                    != (*placement == ExtensionRemoteUiPlacement::Editor)
                {
                    return Err(
                        "remote UI editor mount identity must match editor placement".into(),
                    );
                }
                (surface_id.clone(), Some(geometry))
            }
            ExtensionRemoteUiOperation::Close { surface_id } => {
                serde_json::from_value::<ExtensionRemoteUiCloseResult>(result.clone())
                    .map_err(|error| format!("invalid remote UI close response: {error}"))?;
                (surface_id.clone(), None)
            }
        };
        Ok(Some(RemoteUiResponse {
            mailbox: Arc::clone(&self.mailbox),
            request_id: self.request_id.clone(),
            owner: self.owner.clone(),
            surface_id,
            geometry,
        }))
    }
}

impl Drop for RemoteUiChildRequest {
    fn drop(&mut self) {
        if let ExtensionRemoteUiOperation::Open { surface_id, .. } = &self.operation {
            let mut surfaces = self
                .mailbox
                .surfaces
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let key = (self.owner.clone(), surface_id.clone());
            if surfaces
                .get(&key)
                .is_some_and(|surface| surface.opening.as_ref() == Some(&self.request_id))
            {
                surfaces.remove(&key);
                drop(surfaces);
                self.mailbox.wake();
            }
        }
    }
}

impl RemoteUiResponse {
    /// Runs before the reserved writer slot exposes the acknowledgement to the
    /// child, so its first frame can never race host geometry installation.
    pub(crate) fn commit(self) -> Result<(), String> {
        let mut surfaces = self
            .mailbox
            .surfaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let key = (self.owner, self.surface_id);
        let surface = surfaces
            .get_mut(&key)
            .ok_or_else(|| "remote UI request lost its surface before admission".to_owned())?;
        if let Some(geometry) = self.geometry {
            if surface.opening.as_ref() != Some(&self.request_id) {
                return Err("remote UI open request is no longer current".to_owned());
            }
            surface.opening = None;
            surface.geometry = Some(geometry);
        } else {
            surfaces.remove(&key);
        }
        drop(surfaces);
        self.mailbox.wake();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(session: &str) -> ExtensionResourceOwner {
        ExtensionResourceOwner {
            session_id: session.into(),
            extension_instance_id: "instance".into(),
            process_generation: 1,
        }
    }

    fn frame(surface_id: &str, revision: u64) -> ExtensionRemoteUiFrameNotification {
        ExtensionRemoteUiFrameNotification {
            resource_owner: owner("session"),
            surface_id: surface_id.into(),
            revision,
            columns: 80,
            rows: 24,
            lines: vec![format!("frame {revision}")],
        }
    }

    fn open(
        mailbox: &Arc<RemoteUiMailbox>,
        surface_id: &str,
        request_id: u64,
        parent: Option<u64>,
    ) {
        let request = mailbox
            .reserve(
                ExtensionRequestId::Number(request_id),
                owner("session"),
                ExtensionRemoteUiOperation::Open {
                    surface_id: surface_id.into(),
                    title: "Demo".into(),
                    placement: ExtensionRemoteUiPlacement::Fullscreen,
                    mouse_capture: false,
                },
                parent,
            )
            .unwrap();
        request
            .prepare_response(Some(&serde_json::json!({"columns":80,"rows":24})))
            .unwrap()
            .unwrap()
            .commit()
            .unwrap();
    }

    #[test]
    fn remote_ui_open_placement_and_wire_shapes_are_strict() {
        let mut value =
            serde_json::json!({"parent_request_id":7,"surface_id":"demo","title":"Demo"});
        let request: ExtensionRemoteUiOpenRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(request.placement, ExtensionRemoteUiPlacement::Fullscreen);
        for (wire, placement) in [
            ("fullscreen", ExtensionRemoteUiPlacement::Fullscreen),
            ("header", ExtensionRemoteUiPlacement::Header),
            ("footer", ExtensionRemoteUiPlacement::Footer),
            ("above_editor", ExtensionRemoteUiPlacement::AboveEditor),
            ("below_editor", ExtensionRemoteUiPlacement::BelowEditor),
            ("editor", ExtensionRemoteUiPlacement::Editor),
        ] {
            value["placement"] = wire.into();
            assert_eq!(
                serde_json::from_value::<ExtensionRemoteUiOpenRequest>(value.clone())
                    .unwrap()
                    .placement,
                placement
            );
        }
        for placement in [
            serde_json::Value::Null,
            serde_json::json!("overlay"),
            serde_json::json!(true),
        ] {
            value["placement"] = placement;
            assert!(serde_json::from_value::<ExtensionRemoteUiOpenRequest>(value.clone()).is_err());
        }
        value.as_object_mut().unwrap().remove("placement");
        for parent in [
            serde_json::Value::Null,
            serde_json::json!("7"),
            serde_json::json!(-1),
        ] {
            value["parent_request_id"] = parent;
            assert!(serde_json::from_value::<ExtensionRemoteUiOpenRequest>(value.clone()).is_err());
        }
        let mut frame_value = serde_json::to_value(frame("demo", 0)).unwrap();
        frame_value["resource_owner"]
            .as_object_mut()
            .unwrap()
            .remove("extension_instance_id");
        assert!(serde_json::from_value::<ExtensionRemoteUiFrameNotification>(frame_value).is_err());
    }

    #[test]
    fn remote_ui_identifier_title_geometry_revision_and_sgr_boundaries() {
        let mut operation = ExtensionRemoteUiOperation::Open {
            surface_id: "s".repeat(MAX_EXTENSION_REMOTE_UI_SURFACE_ID_BYTES),
            title: "🦀".repeat(MAX_EXTENSION_REMOTE_UI_TITLE_BYTES / 4),
            placement: ExtensionRemoteUiPlacement::Footer,
            mouse_capture: false,
        };
        operation.validate().unwrap();
        if let ExtensionRemoteUiOperation::Open { title, .. } = &mut operation {
            title.push('x');
        }
        assert_eq!(
            operation.validate().unwrap_err().0,
            ExtensionRequestFailure::BoundsExceeded
        );
        let mut snapshot = frame("demo", MAX_EXTENSION_REMOTE_UI_REVISION);
        snapshot.validate().unwrap();
        snapshot.revision += 1;
        assert_eq!(
            snapshot.validate().unwrap_err().0,
            ExtensionRequestFailure::BoundsExceeded
        );
        snapshot.revision = 0;
        snapshot.columns = 0;
        assert_eq!(
            snapshot.validate().unwrap_err().0,
            ExtensionRequestFailure::InvalidRequest
        );
        let sgr = format!("\u{1b}[{}m", "0".repeat(MAX_EXTENSION_REMOTE_UI_SGR_BYTES));
        validate_remote_ui_line(&sgr).unwrap();
        assert!(validate_remote_ui_line(&format!(
            "\u{1b}[{}m",
            "0".repeat(MAX_EXTENSION_REMOTE_UI_SGR_BYTES + 1)
        ))
        .is_err());
    }

    #[test]
    fn remote_ui_mouse_capture_is_explicit_fullscreen_and_geometry_bounded() {
        let request: ExtensionRemoteUiOpenRequest = serde_json::from_value(serde_json::json!({
            "parent_request_id":7, "surface_id":"demo", "title":"Demo"
        }))
        .unwrap();
        assert!(!request.mouse_capture);
        for placement in ["header", "footer", "above_editor", "below_editor", "editor"] {
            let request: ExtensionRemoteUiOpenRequest = serde_json::from_value(serde_json::json!({
                "parent_request_id":7, "surface_id":"demo", "title":"Demo", "mouse_capture":true, "placement":placement
            })).unwrap();
            let operation = ExtensionRemoteUiOperation::Open {
                surface_id: request.surface_id,
                title: request.title,
                placement: request.placement,
                mouse_capture: request.mouse_capture,
            };
            assert!(operation.validate().is_err());
        }
        let mut mouse: ExtensionRemoteUiMouse = serde_json::from_value(serde_json::json!({
            "surface_id":"demo", "kind":"press", "button":"left", "x":79, "y":23, "modifiers":[]
        }))
        .unwrap();
        assert_eq!(mouse.wheel_delta, 0);
        assert!(mouse.validate().is_ok());
        assert!(
            serde_json::from_value::<ExtensionRemoteUiMouse>(serde_json::json!({
                "surface_id":"demo", "kind":"press", "button":"left", "x":-1, "y":23, "modifiers":[]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ExtensionRemoteUiMouse>(serde_json::json!({
                "surface_id":"demo", "kind":"unknown", "button":"left", "x":0, "y":0, "modifiers":[]
            }))
            .is_err()
        );
        mouse.modifiers = vec![ExtensionRemoteUiKeyModifier::Shift; 2];
        assert!(mouse.validate().is_err());
        mouse.modifiers.clear();
        let mailbox = Arc::new(RemoteUiMailbox::new(Some(Arc::new(Notify::new()))));
        let capture = ExtensionRemoteUiOperation::Open {
            surface_id: "demo".into(),
            title: "Demo".into(),
            placement: ExtensionRemoteUiPlacement::Fullscreen,
            mouse_capture: true,
        };
        let pending = mailbox
            .reserve(ExtensionRequestId::Number(1), owner("one"), capture, None)
            .unwrap();
        assert!(mailbox.validate_mouse(&owner("one"), &mouse).is_err());
        pending
            .prepare_response(Some(&serde_json::json!({"columns":80,"rows":24})))
            .unwrap()
            .unwrap()
            .commit()
            .unwrap();
        assert!(mailbox.validate_mouse(&owner("one"), &mouse).is_ok());
        assert!(mailbox.validate_mouse(&owner("other"), &mouse).is_err());
        mouse.x = 80;
        assert!(mailbox.validate_mouse(&owner("one"), &mouse).is_err());
        mouse.x = 0;
        mouse.y = 24;
        assert!(mailbox.validate_mouse(&owner("one"), &mouse).is_err());
        mailbox.close(&owner("one"), "demo");
        let no_capture = ExtensionRemoteUiOperation::Open {
            surface_id: "demo".into(),
            title: "Demo".into(),
            placement: ExtensionRemoteUiPlacement::Fullscreen,
            mouse_capture: false,
        };
        let pending = mailbox
            .reserve(
                ExtensionRequestId::Number(2),
                owner("one"),
                no_capture,
                None,
            )
            .unwrap();
        pending
            .prepare_response(Some(&serde_json::json!({"columns":80,"rows":24})))
            .unwrap()
            .unwrap()
            .commit()
            .unwrap();
        mouse.y = 0;
        assert!(mailbox.validate_mouse(&owner("one"), &mouse).is_err());
    }

    #[test]
    fn remote_ui_mailboxes_are_latest_wins_and_keep_revision_after_drain() {
        let mailbox = Arc::new(RemoteUiMailbox::new(None));
        open(&mailbox, "demo", 1, Some(7));
        open(&mailbox, "footer", 2, Some(7));
        for revision in 0..1000 {
            mailbox.accept(frame("demo", revision), 1).unwrap();
        }
        mailbox.accept(frame("footer", 0), 1).unwrap();
        let snapshots = mailbox.take_frames();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots
            .iter()
            .any(|frame| frame.surface_id == "demo" && frame.revision == 999));
        assert!(mailbox.take_frames().is_empty());
        assert!(mailbox.accept(frame("demo", 999), 1).is_err());
        let resize = ExtensionRemoteUiResize {
            surface_id: "demo".into(),
            columns: 90,
            rows: 30,
        };
        mailbox.resize(&owner("session"), &resize);
        assert!(mailbox.accept(frame("demo", 1000), 1).is_err());
        let mut resized = frame("demo", 1000);
        resized.columns = 90;
        resized.rows = 30;
        mailbox.accept(resized, 1).unwrap();
        mailbox.settle_parent(7, false);
        mailbox.settle_parent(7, true);
        assert!(
            mailbox.contains(&owner("session"), "demo"),
            "normal settlement detached the command"
        );
        mailbox.discard_owner(&owner("session"));
        assert!(mailbox.take_frames().is_empty());
    }

    #[test]
    fn remote_ui_pending_slots_cancellation_and_owner_identity_are_bounded() {
        let mailbox = Arc::new(RemoteUiMailbox::new(None));
        let mut requests = Vec::new();
        for id in 0..MAX_EXTENSION_REMOTE_UI_SURFACES {
            requests.push(
                mailbox
                    .reserve(
                        ExtensionRequestId::Number(id as u64),
                        owner("session"),
                        ExtensionRemoteUiOperation::Open {
                            surface_id: format!("s{id}"),
                            title: "Demo".into(),
                            placement: ExtensionRemoteUiPlacement::Footer,
                            mouse_capture: false,
                        },
                        Some(7),
                    )
                    .unwrap(),
            );
        }
        let extra = || {
            mailbox.reserve(
                ExtensionRequestId::Number(100),
                owner("other"),
                ExtensionRemoteUiOperation::Open {
                    surface_id: "extra".into(),
                    title: "Demo".into(),
                    placement: ExtensionRemoteUiPlacement::Footer,
                    mouse_capture: false,
                },
                Some(8),
            )
        };
        assert!(matches!(
            extra(),
            Err((ExtensionRequestFailure::BoundsExceeded, _))
        ));
        let prepared = requests[0]
            .prepare_response(Some(&serde_json::json!({"columns":80,"rows":24})))
            .unwrap()
            .unwrap();
        requests.remove(0);
        assert!(
            prepared.commit().is_err(),
            "cancelled reservation cannot be revived by a late reply"
        );
        let request = extra().unwrap();
        assert!(
            mailbox.accept(frame("s1", 0), 1).is_err(),
            "no frames before actual admission"
        );
        mailbox.settle_parent(7, true);
        assert!(!mailbox.contains(&owner("session"), "s1"));
        drop(request);
        assert!(mailbox.owners().is_empty());
        open(&mailbox, "same", 101, None);
        let mut foreign = frame("same", 0);
        foreign.resource_owner.session_id = "other".into();
        assert!(mailbox.accept(foreign, 1).is_err());
        mailbox.clear();
        assert!(mailbox.host_owner("same").is_err());
    }

    #[test]
    fn remote_ui_keys_and_closure_reasons_reject_controls_and_duplicate_modifiers() {
        let mut key = ExtensionRemoteUiKey {
            surface_id: "demo".into(),
            key: "x".repeat(MAX_EXTENSION_REMOTE_UI_KEY_BYTES),
            kind: ExtensionRemoteUiKeyKind::Release,
            modifiers: vec![ExtensionRemoteUiKeyModifier::Control],
            editor_input: None,
        };
        key.validate().unwrap();
        key.key.push('x');
        assert_eq!(
            key.validate().unwrap_err().0,
            ExtensionRequestFailure::BoundsExceeded
        );
        key.key = "Enter".into();
        key.modifiers.push(ExtensionRemoteUiKeyModifier::Control);
        assert_eq!(
            key.validate().unwrap_err().0,
            ExtensionRequestFailure::InvalidRequest
        );
        let mut closed = ExtensionRemoteUiClosed {
            surface_id: "demo".into(),
            reason: "x".repeat(MAX_EXTENSION_REMOTE_UI_REASON_BYTES),
        };
        closed.validate().unwrap();
        closed.reason = "\u{1b}[2J".into();
        assert_eq!(
            closed.validate().unwrap_err().0,
            ExtensionRequestFailure::InvalidRequest
        );
    }

    #[test]
    fn remote_ui_lines_accept_only_printable_text_and_checked_sgr() {
        for line in [
            "",
            "Hello 🦀",
            "\u{1b}[0m",
            "\u{1b}[1;4;31;49mred\u{1b}[0m",
            "\u{1b}[38;5;255;48;2;0;128;255m▀\u{1b}[0m",
        ] {
            validate_remote_ui_line(line).expect("safe printable SGR line");
        }
        for line in [
            "\n",
            "\r",
            "\t",
            "\0",
            "\u{7f}",
            "\u{85}",
            "\u{9b}31m",
            "\u{2028}",
            "\u{1b}]0;title\u{7}",
            "\u{1b}[2J",
            "\u{1b}[H",
            "\u{1b}[31m\u{1b}[2K",
            "\u{1b}7",
            "\u{1b}[m",
            "\u{1b}[31; m",
            "\u{1b}[38;5;256m",
            "\u{1b}[48;2;0;256;0m",
            "\u{1b}[38;2;0;0m",
            "\u{1b}[38:2:0:0:0m",
            "\u{1b}[999m",
            "\u{1b}[+31m",
            "\u{1b}[31;m",
            "\u{1b}[31m\u{1b}",
        ] {
            assert!(
                validate_remote_ui_line(line).is_err(),
                "unsafe line: {line:?}"
            );
        }
    }

    #[test]
    fn remote_editor_mount_fence_survives_resize_and_key_metadata_is_bounded() {
        let mailbox = Arc::new(RemoteUiMailbox::new(None));
        let request = mailbox
            .reserve(
                ExtensionRequestId::Number(1),
                owner("session"),
                ExtensionRemoteUiOperation::Open {
                    surface_id: "editor".into(),
                    title: "Editor".into(),
                    placement: ExtensionRemoteUiPlacement::Editor,
                    mouse_capture: false,
                },
                None,
            )
            .unwrap();
        assert!(request
            .prepare_response(Some(&serde_json::json!({"columns":80,"rows":24})))
            .is_err());
        request
            .prepare_response(Some(&serde_json::json!({
                "columns":80,"rows":24,"editor_mount_id":"remote.7"
            })))
            .unwrap()
            .unwrap()
            .commit()
            .unwrap();
        mailbox.resize(
            &owner("session"),
            &ExtensionRemoteUiResize {
                surface_id: "editor".into(),
                columns: 90,
                rows: 30,
            },
        );
        let geometry = mailbox
            .surfaces
            .lock()
            .unwrap()
            .get(&(owner("session"), "editor".into()))
            .unwrap()
            .geometry
            .clone()
            .unwrap();
        assert_eq!((geometry.columns, geometry.rows), (90, 30));
        assert_eq!(geometry.editor_mount_id.as_deref(), Some("remote.7"));
        let plain: ExtensionRemoteUiKey = serde_json::from_value(serde_json::json!({
            "surface_id":"editor","key":"a","kind":"press","modifiers":[]
        }))
        .unwrap();
        assert!(serde_json::to_value(&plain)
            .unwrap()
            .get("editor_input")
            .is_none());
        let mut key = plain;
        key.editor_input = Some(ExtensionRemoteUiEditorInput {
            mount_id: "remote.7".into(),
            input_revision: 1,
        });
        key.validate().unwrap();
        for revision in [0, MAX_EXTENSION_REMOTE_UI_REVISION + 1] {
            key.editor_input.as_mut().unwrap().input_revision = revision;
            assert!(key.validate().is_err());
        }
        key.editor_input.as_mut().unwrap().input_revision = MAX_EXTENSION_REMOTE_UI_REVISION;
        key.validate().unwrap();
        key.editor_input.as_mut().unwrap().mount_id = "bad/mount".into();
        assert!(key.validate().is_err());
        let mut unknown = serde_json::to_value(&key).unwrap();
        unknown["editor_input"]["extra"] = true.into();
        assert!(serde_json::from_value::<ExtensionRemoteUiKey>(unknown).is_err());
    }

    #[test]
    fn remote_ui_frame_bounds_accept_exact_limits_and_reject_excess() {
        let mut frame = ExtensionRemoteUiFrameNotification {
            resource_owner: ExtensionResourceOwner {
                session_id: "session".into(),
                extension_instance_id: "instance".into(),
                process_generation: 1,
            },
            surface_id: "screen".into(),
            revision: 0,
            columns: 80,
            rows: 24,
            lines: vec!["x".repeat(MAX_EXTENSION_REMOTE_UI_LINE_BYTES); 32],
        };
        frame
            .validate()
            .expect("exact aggregate and per-line bounds");
        frame.lines.push("x".into());
        assert_eq!(
            frame.validate().unwrap_err().0,
            ExtensionRequestFailure::BoundsExceeded
        );
        frame.lines = vec!["".into(); MAX_EXTENSION_REMOTE_UI_LINES];
        frame.validate().expect("exact line count bound");
        frame.lines.push("".into());
        assert_eq!(
            frame.validate().unwrap_err().0,
            ExtensionRequestFailure::BoundsExceeded
        );
        frame.lines = vec!["x".repeat(MAX_EXTENSION_REMOTE_UI_LINE_BYTES + 1)];
        assert_eq!(
            frame.validate().unwrap_err().0,
            ExtensionRequestFailure::BoundsExceeded
        );
    }
}

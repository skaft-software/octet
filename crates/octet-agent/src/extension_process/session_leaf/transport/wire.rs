//! Closed v2 wire models and the effective physical-envelope calculation.
use super::*;

/// Exact host-offered json-chunks.v1 encoded-byte and record quotas.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshotProfile {
    /// Closed profile name.
    pub profile: String,
    /// Decoded bytes per chunk (not JSON/base64 bytes).
    pub chunk_bytes: usize,
    /// Maximum exact encoded history document.
    pub snapshot_bytes: usize,
    /// Simultaneously retained encoded history bytes.
    pub generation_bytes: usize,
    /// Simultaneously retained current and pinned views.
    pub owner_views: usize,
    /// Independently charged read handles.
    pub transfers: usize,
    /// Maximum records in one view.
    pub view_entries: usize,
    /// Simultaneously retained entry and branch-ID records.
    pub generation_entries: usize,
    /// Independent canonical projection staging quota.
    pub projection_bytes: usize,
    /// Simultaneously staged projections.
    pub projections: usize,
}
impl SessionSnapshotProfile {
    pub(crate) fn for_frame(frame: usize) -> Result<Self, ExtensionRuntimeError> {
        // A legacy frame limit includes LF; v2 advertises the effective JSON
        // envelope capacity. IDs may contain maximally escaped characters.
        let frame = frame.saturating_sub(1);
        let id = "\u{0001}".repeat(256);
        let token = "f".repeat(64);
        let chunk_fits = |bytes: usize| {
            let data = "A".repeat(bytes.div_ceil(3) * 4);
            let reply = json!({"jsonrpc":"2.0","id":id,"result":{"transfer_id":token,"offset":MAX_SAFE,"data":data,"next_offset":MAX_SAFE,"eof":false}});
            let request = json!({"jsonrpc":"2.0","id":id,"method":PROJECTION_CHUNK,"params":{"parent_request_id":MAX_SAFE,"transfer_id":token,"offset":MAX_SAFE,"data":data}});
            validate_session_snapshot_size(&reply, frame).is_ok()
                && validate_session_snapshot_size(&request, frame).is_ok()
        };
        let mut low = 0usize;
        let mut high = 65_536usize;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if chunk_fits(mid) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        if low == 0 {
            return Err(unavailable());
        }
        let profile = Self {
            profile: "json-chunks.v1".into(),
            chunk_bytes: low,
            snapshot_bytes: 268_435_456,
            generation_bytes: 536_870_912,
            owner_views: 64,
            transfers: 64,
            view_entries: 1_048_576,
            generation_entries: 2_097_152,
            projection_bytes: 67_108_864,
            projections: 64,
        };
        let owner = ExtensionResourceOwner {
            session_id: "\u{0001}".repeat(256),
            extension_instance_id: "\u{0001}".repeat(256),
            process_generation: MAX_SAFE,
        };
        let descriptor = Descriptor {
            transfer_id: token.clone(),
            kind: "history_delta".into(),
            owner,
            view_revision: MAX_SAFE,
            head: Some("\u{0001}".repeat(256)),
            bytes: profile.snapshot_bytes,
            sha256: token,
            entry_count: profile.view_entries,
            branch_count: profile.view_entries,
            preparation: Some(Preparation {
                activation_epoch: MAX_SAFE,
                operation_id: "\u{0001}".repeat(256),
                tool_generation: MAX_SAFE,
                head: Some("\u{0001}".repeat(256)),
            }),
        };
        for envelope in [
            json!({"jsonrpc":"2.0","id":MAX_SAFE,"method":PREPARE,"params":{"resource_owner":descriptor.owner,"snapshot":descriptor,"host":{}}}),
            json!({"jsonrpc":"2.0","id":id,"result":{"version":"0.4","features":[OWNER_ROUTES,SNAPSHOT_TRANSPORT,"session_entries","request_cancellation"],"limits":{"max_concurrent_requests":64,"max_message_bytes":frame},"session_snapshot_transport_v1":profile}}),
        ] {
            validate_session_snapshot_size(&envelope, frame)?;
        }
        Ok(profile)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct Preparation {
    pub activation_epoch: u64,
    pub operation_id: String,
    pub tool_generation: u64,
    pub head: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct Descriptor {
    pub transfer_id: String,
    pub kind: String,
    pub owner: ExtensionResourceOwner,
    pub view_revision: u64,
    pub head: Option<String>,
    pub bytes: usize,
    pub sha256: String,
    pub entry_count: usize,
    pub branch_count: usize,
    pub preparation: Option<Preparation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct Parent {
    pub parent_request_id: u64,
}
impl OwnerScopedHostRequest for Parent {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct Read {
    pub parent_request_id: u64,
    pub transfer_id: String,
    pub offset: usize,
    pub max_bytes: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct Handle {
    pub parent_request_id: u64,
    pub transfer_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct ProjectionBegin {
    pub parent_request_id: u64,
    pub bytes: usize,
    pub sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct ProjectionChunk {
    pub parent_request_id: u64,
    pub transfer_id: String,
    pub offset: usize,
    pub data: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::extension_process) struct ProjectionResult {
    pub transfer_id: String,
    pub bytes: usize,
    pub sha256: String,
}
pub(in crate::extension_process) fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

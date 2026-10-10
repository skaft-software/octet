//! Host-owned exact nominal operation lookup. Discovery changes presentation,
//! never resource ownership, invocation arguments, effects or execution policy.

use std::collections::BTreeSet;
use std::sync::{RwLock, Weak};

use octet_ai::ToolDef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::effect::ToolEffect;
use crate::extension::DynamicToolRegistry;
use crate::extension_process::{ExtensionResourceOwner, OperationDescriptor, ResourceRef};
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};

/// One immutable operation catalog entry's identity and negotiated metadata.
#[derive(Clone, Debug)]
pub struct OperationSnapshot {
    /// Wire descriptor belonging to the captured schema and handler.
    pub descriptor: OperationDescriptor,
    /// Issuing process instance, never a model-supplied authority.
    pub extension_instance_id: String,
    /// Issuing process generation.
    pub generation: u64,
    /// Catalog revision captured with the handler.
    pub catalog_revision: u64,
}

/// One bounded host lookup. There is deliberately no semantic-query contract.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicableOperationsRequest {
    /// An already active reference, validated under the caller's owner.
    pub resource: ResourceRef,
    /// Number of matching slots, default 8, maximum 32.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Opaque continuation from the preceding page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// A matching input slot; other required inputs have NOT been supplied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApplicableOperation {
    /// Stable nominal operation identifier.
    pub id: String,
    /// Ordinary tool/call dispatch name.
    pub tool: String,
    /// Exact matching declared input JSON Pointer.
    pub path: String,
    /// Whether this slot is marked as the preferred presentation receiver.
    pub primary_receiver: bool,
    /// Schema/handler revision from the catalog snapshot.
    pub catalog_revision: u64,
}

/// Bounded lookup result. Selected schemas appear on the next model request.
#[derive(Clone, Debug, Serialize)]
pub struct ApplicableOperationsPage {
    /// Ordered by operation ID, then matching input path.
    pub operations: Vec<ApplicableOperation>,
    /// Absent when the eligible snapshot has no more slots.
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub(crate) struct OperationSelection {
    pub(crate) owner: ExtensionResourceOwner,
    pub(crate) resource: ResourceRef,
    pub(crate) ids: BTreeSet<String>,
}

impl OperationSelection {
    pub(crate) fn includes(&self, operation: &OperationSnapshot, owner: &str) -> bool {
        self.owner.session_id == owner
            && self.owner.extension_instance_id == operation.extension_instance_id
            && self.owner.process_generation == operation.generation
            && self.ids.contains(&operation.descriptor.id)
            && operation
                .descriptor
                .resource_inputs
                .iter()
                .any(|slot| slot.resource_type == self.resource.resource_type)
    }
}

pub(crate) fn matching_slots(
    operation: &OperationSnapshot,
    tool: &str,
    owner: &ExtensionResourceOwner,
    resource: &ResourceRef,
) -> Vec<ApplicableOperation> {
    if operation.extension_instance_id != owner.extension_instance_id
        || operation.generation != owner.process_generation
    {
        return Vec::new();
    }
    operation
        .descriptor
        .resource_inputs
        .iter()
        .filter(|slot| slot.resource_type == resource.resource_type)
        .map(|slot| ApplicableOperation {
            id: operation.descriptor.id.clone(),
            tool: tool.to_owned(),
            path: slot.path.clone(),
            primary_receiver: operation.descriptor.receiver.as_ref() == Some(&slot.path),
            catalog_revision: operation.catalog_revision,
        })
        .collect()
}

// The digest exposes neither session identity nor native process internals.
// A cursor is only an offset, not an authority token: every page revalidates
// the live reference and current policy before inspecting the eligible set.
pub(crate) fn cursor_binding(
    owner: &ExtensionResourceOwner,
    resource: &ResourceRef,
    revision: u64,
) -> String {
    let bytes =
        serde_json::to_vec(&(owner, resource, revision)).expect("serializable cursor binding");
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn cursor_offset(cursor: Option<&str>, binding: &str) -> Result<usize, ToolError> {
    let Some(cursor) = cursor else { return Ok(0) };
    let (_, offset) = cursor
        .split_once(':')
        .filter(|(prefix, _)| *prefix == binding)
        .ok_or_else(|| ToolError::new("catalog_changed"))?;
    offset
        .parse()
        .map_err(|_| ToolError::new("catalog_changed"))
}

pub(crate) const DISCOVERY_TOOL_NAME: &str = "get_applicable_operations";

pub(crate) struct OperationDiscovery {
    pub(crate) registry: Weak<RwLock<DynamicToolRegistry>>,
}

#[async_trait::async_trait]
impl Tool for OperationDiscovery {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: DISCOVERY_TOOL_NAME.into(),
            description: "Find exact operations for a live ResourceRef. Returns at most 8 matching input slots by default (maximum 32), ordered by operation ID/path, and projects their exact tool schemas for the next turn. Other required arguments must still be supplied. Discovery never invokes an operation or grants permission.".into(),
            parameters: serde_json::json!({
                "type":"object", "additionalProperties":false,
                "properties":{
                    "resource":{
                        "type":"object", "additionalProperties":false,
                        "properties":{
                            "$resource":{"type":"string","minLength":1,"maxLength":128},
                            "type":{"type":"string","minLength":1,"maxLength":128}
                        },
                        "required":["$resource","type"]
                    },
                    "limit":{"type":"integer","minimum":1,"maximum":32},
                    "cursor":{"type":"string","maxLength":96}
                },
                "required":["resource"]
            }),
            async_execution: false,
            constrained_sampling: None,
        }
    }

    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        // Only in-memory host-owned metadata is read. Projection confers no
        // authority; the selected operation still undergoes normal admission.
        Ok(ToolEffect::Pure)
    }

    fn composition_is_unmetered(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let request = serde_json::from_value(args)
            .map_err(|_| ToolError::new("invalid applicable-operation request"))?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| ToolError::new("resource_unavailable"))?;
        let page = DynamicToolRegistry::discover(&registry, ctx.resource_owner, request)?;
        let value = serde_json::to_value(page).expect("serializable operation page");
        ToolOutput::new(value.to_string())
            .try_with_programmatic_content(value)
            .map_err(|error| ToolError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests;

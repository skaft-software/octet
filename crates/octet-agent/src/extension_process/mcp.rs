//! Bounded owner-scoped admission for transient resident MCP registrations.
use super::*;

/// A complete Pi registry snapshot. The resident MCP extension validates the
/// native launch profile; this type never starts or resolves a transport.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMcpRequest {
    /// Live host parent that authorizes this reverse request.
    pub parent_request_id: u64,
    /// Exact previously issued session, instance and process generation.
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Configs are sensitive; never echo this payload in diagnostics.
    pub servers: Vec<serde_json::Value>,
}

impl std::fmt::Debug for ExtensionMcpRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionMcpRequest")
            .field("parent_request_id", &self.parent_request_id)
            .field("resource_owner", &self.resource_owner)
            .field("server_count", &self.servers.len())
            .finish_non_exhaustive()
    }
}

impl ExtensionMcpRequest {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.servers.len() > 32
            || serde_json::to_vec(&self.servers)
                .map_err(|_| "invalid MCP snapshot")?
                .len()
                > 262144
        {
            return Err("MCP registration snapshot exceeds bounds".into());
        }
        let mut namespaces = std::collections::BTreeSet::new();
        for server in &self.servers {
            let object = server
                .as_object()
                .ok_or("invalid MCP registration record")?;
            let name = object
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("invalid MCP registration name")?;
            if object.len() != 3
                || name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                || !object
                    .get("config")
                    .is_some_and(serde_json::Value::is_object)
                || !object
                    .get("extensionPath")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| !path.is_empty() && path.len() <= 4096)
                || !namespaces.insert(name.replace('-', "_"))
            {
                return Err("invalid MCP registration record or namespace collision".into());
            }
        }
        Ok(())
    }
}

pub(super) fn dispatch_mcp_registration(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let Some((request, admitted)) = admit_host_request::<ExtensionMcpRequest>(
        state,
        object,
        "mcp/replace",
        "mcp_registration_v1",
        params,
    )?
    else {
        return Ok(());
    };
    if let Err(failure) = validate_explicit_request_owner(state, &admitted.owner) {
        return refuse_admitted_request(state, &admitted, failure);
    }
    if let Err(detail) = request.validate() {
        return refuse_admitted_request(
            state,
            &admitted,
            (ExtensionRequestFailure::InvalidRequest, detail),
        );
    }
    dispatch_host_request_event(state, &admitted, |admitted| {
        ExtensionEvent::McpRegistrationRequested {
            request_id: admitted.request_id.clone(),
            generation: admitted.generation,
            owner: admitted.owner.clone(),
            request,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(servers: serde_json::Value) -> ExtensionMcpRequest {
        serde_json::from_value(serde_json::json!({
            "parent_request_id": 1,
            "resource_owner": null,
            "servers": servers,
        }))
        .unwrap()
    }

    #[test]
    fn mcp_snapshot_bounds_namespaces_and_debug_are_sensitive_safe() {
        let entry = serde_json::json!({"name":"proof-one", "extensionPath":"/reviewed/pi.mjs", "config":{"command":"/server", "env":{"TOKEN":"PRIVATE_SECRET"}}});
        let accepted = request(serde_json::json!([entry.clone()]));
        assert!(accepted.validate().is_ok());
        assert!(!format!("{accepted:?}").contains("PRIVATE_SECRET"));
        let mut collided = entry.clone();
        collided["name"] = "proof_one".into();
        assert!(request(serde_json::json!([entry.clone(), collided]))
            .validate()
            .is_err());
        assert!(request(serde_json::json!(vec![entry; 33]))
            .validate()
            .is_err());
    }

    #[test]
    fn mcp_request_cannot_supply_the_host_privilege_marker() {
        assert!(
            serde_json::from_value::<ExtensionMcpRequest>(serde_json::json!({
                "parent_request_id":1, "resource_owner":null, "servers":[],
                "mcp_registration_owner":{"session_id":"forged"}
            }))
            .is_err()
        );
    }
}

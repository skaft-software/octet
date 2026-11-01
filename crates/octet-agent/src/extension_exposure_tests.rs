use super::*;

#[test]
fn pi_tool_exposure_wire_accepts_all_modes_and_refuses_unknown() {
    for exposure in ["direct", "codemode", "deferred", "model-only", "hidden"] {
        let value = serde_json::json!({"name":"probe","description":"Probe","parameters":{"type":"object"},"exposure":exposure});
        let definition: crate::extension_process::ToolDefinition =
            serde_json::from_value(value).unwrap();
        assert_eq!(
            serde_json::to_value(definition).unwrap()["exposure"],
            exposure
        );
    }
    assert!(serde_json::from_value::<crate::extension_process::ToolDefinition>(serde_json::json!({"name":"probe","description":"Probe","parameters":{},"exposure":"bad"})).is_err());
}

struct ExposedTool(&'static str, crate::tool::ToolExposure);
#[async_trait::async_trait]
impl crate::tool::Tool for ExposedTool {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: self.0.into(),
            description: "Probe".into(),
            parameters: serde_json::json!({"type":"object"}),
            constrained_sampling: None,
            async_execution: false,
        }
    }
    fn exposure(&self) -> crate::tool::ToolExposure {
        self.1
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: &crate::tool::ToolContext<'_>,
    ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
        Ok(crate::tool::ToolOutput::new("probe"))
    }
}

fn host() -> ExtensionHost {
    use crate::tool::ToolExposure::*;
    let mut host = ExtensionHost::new();
    for (name, exposure) in [
        ("direct", Direct),
        ("codemode", Codemode),
        ("deferred", Deferred),
        ("model_only", ModelOnly),
        ("hidden", Hidden),
    ] {
        host.tool(ExposedTool(name, exposure));
    }
    host
}
fn names(tools: &[Arc<dyn crate::tool::Tool>]) -> Vec<String> {
    tools.iter().map(|tool| tool.definition().name).collect()
}

#[test]
fn pi_tool_exposure_model_and_callable_surfaces_are_distinct_and_coherent() {
    let host = host();
    let (revision, model, callable) = host.model_and_callable_tool_snapshot("owner");
    assert_eq!(names(&model), ["direct", "model_only"]);
    assert_eq!(names(&callable), ["direct", "codemode", "deferred"]);
    assert_eq!(
        host.pi_tool_snapshot()["active_tools"],
        serde_json::json!(["direct", "model_only"])
    );
    host.set_active_tools(Some(&BTreeSet::from([
        "codemode".into(),
        "model_only".into(),
        "hidden".into(),
    ])))
    .unwrap();
    let (next, model, callable) = host.model_and_callable_tool_snapshot("owner");
    assert!(next > revision);
    assert_eq!(names(&model), ["codemode", "model_only"]);
    assert_eq!(names(&callable), ["codemode", "deferred"]);
    assert_eq!(
        host.pi_tool_snapshot()["all_tools"][4]["exposure"],
        "hidden"
    );
    assert_eq!(
        host.pi_tool_snapshot()["active_tools"],
        serde_json::json!(["codemode", "model_only"])
    );
    host.set_active_tools(Some(&BTreeSet::new())).unwrap();
    assert!(host.model_and_callable_tool_snapshot("owner").1.is_empty());
    assert_eq!(
        names(&host.model_and_callable_tool_snapshot("owner").2),
        ["codemode", "deferred"]
    );
    host.set_active_tools(None).unwrap();
    assert_eq!(
        names(&host.model_and_callable_tool_snapshot("owner").1),
        ["direct", "model_only"]
    );
}

#[test]
fn pi_tool_exposure_never_restores_policy_denied_or_retired_tools() {
    let mut host = host();
    host.set_tool_policy(|name| name != "codemode");
    let (_, model, callable) = host.model_and_callable_tool_snapshot("owner");
    assert_eq!(names(&model), ["direct", "model_only"]);
    assert_eq!(names(&callable), ["direct", "deferred"]);
    assert!(host
        .set_active_tools(Some(&BTreeSet::from(["codemode".into()])))
        .is_err());
    assert_eq!(
        names(&host.model_and_callable_tool_snapshot("owner").2),
        ["direct", "deferred"]
    );
    host.retain_tools(|name| name != "deferred");
    assert_eq!(
        names(&host.model_and_callable_tool_snapshot("owner").2),
        ["direct"]
    );
}

#[test]
fn pi_tool_exposure_dynamic_publication_preserves_static_defaults_and_retirement() {
    use crate::tool::ToolExposure::Deferred;
    let mut host = host();
    let registration = host
        .dynamic_tools("extension", vec![Arc::new(ExposedTool("late", Deferred))])
        .unwrap();
    assert_eq!(
        names(&host.model_and_callable_tool_snapshot("owner").1),
        ["direct", "model_only"]
    );
    assert_eq!(
        names(&host.model_and_callable_tool_snapshot("owner").2),
        ["direct", "codemode", "deferred", "late"]
    );
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    registration
        .reserve_for_generation(
            vec![Arc::new(ExposedTool("late", Deferred))],
            Some(Arc::clone(&closed)),
        )
        .unwrap()
        .commit()
        .unwrap();
    closed.store(true, Ordering::Release);
    assert_eq!(
        names(&host.model_and_callable_tool_snapshot("owner").2),
        ["direct", "codemode", "deferred"]
    );
    host.set_active_tools(Some(&BTreeSet::from(["codemode".into()])))
        .unwrap();
    let (child, effective) = host
        .scoped_tool_snapshot(&BTreeSet::from(["codemode".into()]))
        .unwrap();
    assert_eq!(effective, ["codemode"]);
    assert_eq!(
        names(&child.model_and_callable_tool_snapshot("owner").1),
        ["codemode"]
    );
}

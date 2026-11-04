//! Effective builtin replacement is an explicitly authorized catalog transaction.
use super::*;
use crate::effect::ToolEffect;
use crate::tool::{ReplaySafety, ToolConcurrency, ToolOutput, ToolPromptContribution};
use std::sync::atomic::{AtomicBool, Ordering};

struct Replacement(&'static str);

#[async_trait::async_trait]
impl Tool for Replacement {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: self.0.into(),
            description: "extension replacement".into(),
            parameters: serde_json::json!({"type":"object","properties":{"anchor":{"type":"string"}},"required":["anchor"]}),
            async_execution: false,
            constrained_sampling: None,
        }
    }

    fn prompt_metadata(&self) -> Option<ToolPromptContribution> {
        Some(ToolPromptContribution {
            name: self.0.into(),
            snippet: "Use extension anchors".into(),
            guidelines: vec!["Never use the displaced schema".into()],
        })
    }

    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Extension)
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        _: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("replacement"))
    }
}

fn base_host() -> ExtensionHost {
    let mut host = ExtensionHost::new();
    host.load(&crate::tools::CoreTools);
    host
}

fn read_tool(host: &ExtensionHost) -> Arc<dyn Tool> {
    host.tool_snapshot()
        .1
        .into_iter()
        .find(|tool| tool.definition().name == "read")
        .unwrap()
}

fn publish(
    host: &mut ExtensionHost,
    owner: &str,
    closed: Option<Arc<AtomicBool>>,
) -> DynamicToolRegistration {
    host.dynamic_tools_with_authority(
        owner,
        vec![Arc::new(Replacement("read"))],
        BTreeSet::from(["read".into()]),
        closed,
        |_, _| {},
    )
    .unwrap()
}

fn pi_tool<'a>(snapshot: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    let mut tools = snapshot["all_tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["name"] == name);
    let tool = tools.next().unwrap();
    assert!(tools.next().is_none());
    tool
}

#[test]
fn pi_tool_snapshot_sibling_provenance_ordinary_before_core() {
    let template = ExtensionHost::new();
    let mut ordinary = template.clone();
    ordinary.tool(crate::tools::ReadTool);
    let before = ordinary.pi_tool_snapshot();
    let original = read_tool(&ordinary);
    let definitions = serde_json::to_value(ordinary.tool_definitions()).unwrap();
    assert_eq!(pi_tool(&before, "read")["sourceInfo"]["source"], "native");
    assert_eq!(
        pi_tool(&before, "read")["sourceInfo"]["path"],
        "<native:read>"
    );

    let mut core = template.clone();
    core.load(&crate::tools::CoreTools);
    assert!(ordinary.duplicate_tools.is_empty());
    assert!(core.duplicate_tools.is_empty());
    assert!(Arc::ptr_eq(&original, &read_tool(&ordinary)));
    assert_eq!(
        serde_json::to_value(ordinary.tool_definitions()).unwrap(),
        definitions
    );
    let after = ordinary.pi_tool_snapshot();
    assert_eq!(after["active_tools"], before["active_tools"]);
    assert_eq!(after, before);
    let core_snapshot = core.pi_tool_snapshot();
    assert_eq!(
        pi_tool(&core_snapshot, "read")["sourceInfo"]["source"],
        "builtin"
    );
    assert_eq!(
        pi_tool(&core_snapshot, "read")["sourceInfo"]["path"],
        "builtin:read"
    );
}

#[test]
fn pi_tool_snapshot_sibling_provenance_core_before_ordinary() {
    let template = ExtensionHost::new();
    let mut core = template.clone();
    core.load(&crate::tools::CoreTools);
    let before = core.pi_tool_snapshot();
    let mut ordinary = template.clone();
    ordinary.tool(crate::tools::ReadTool);
    assert!(ordinary.duplicate_tools.is_empty());
    let snapshot = ordinary.pi_tool_snapshot();
    assert_eq!(pi_tool(&snapshot, "read")["sourceInfo"]["source"], "native");
    assert_eq!(
        pi_tool(&snapshot, "read")["sourceInfo"]["path"],
        "<native:read>"
    );
    assert_eq!(core.pi_tool_snapshot(), before);
    assert_eq!(pi_tool(&before, "read")["sourceInfo"]["source"], "builtin");
}

#[test]
fn pi_tool_snapshot_sibling_provenance_builtin_clone_is_local() {
    let host = base_host();
    let clone = host.clone();
    let before = host.pi_tool_snapshot();
    assert_eq!(clone.pi_tool_snapshot(), before);
    assert!(Arc::ptr_eq(&read_tool(&host), &read_tool(&clone)));
    let mut sibling = host.clone();
    sibling.retain_tools(|name| name != "read");
    sibling.tool(crate::tools::ReadTool);
    sibling.tool(Replacement("custom"));
    assert!(sibling.duplicate_tools.is_empty());
    assert_eq!(host.pi_tool_snapshot(), before);
    assert_eq!(clone.pi_tool_snapshot(), before);
    let sibling_snapshot = sibling.pi_tool_snapshot();
    assert_eq!(
        pi_tool(&sibling_snapshot, "read")["sourceInfo"]["source"],
        "native"
    );
    assert_eq!(
        pi_tool(&sibling_snapshot, "custom")["sourceInfo"]["source"],
        "native"
    );
}

#[test]
fn pi_tool_snapshot_sibling_provenance_same_arc_ordinary_reregistration() {
    let mut host = base_host();
    let original = read_tool(&host);
    host.retain_tools(|name| name != "read");
    host.tool_arc(Arc::clone(&original));
    assert!(host.duplicate_tools.is_empty());
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
    let snapshot = host.pi_tool_snapshot();
    assert_eq!(pi_tool(&snapshot, "read")["sourceInfo"]["source"], "native");
    assert_eq!(
        pi_tool(&snapshot, "read")["sourceInfo"]["path"],
        "<native:read>"
    );
}

#[test]
fn pi_tool_snapshot_sibling_provenance_refused_registration_keeps_identity() {
    let mut host = base_host();
    let before = host.pi_tool_snapshot();
    let original = read_tool(&host);
    host.tool_arc(Arc::clone(&original));
    assert_eq!(host.duplicate_tools, ["read"]);
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
    assert_eq!(host.pi_tool_snapshot(), before);

    let mut ordinary = ExtensionHost::new();
    ordinary.tool(crate::tools::ReadTool);
    let before = ordinary.pi_tool_snapshot();
    ordinary.builtin_tool(crate::tools::ReadTool);
    assert_eq!(ordinary.duplicate_tools, ["read"]);
    assert_eq!(ordinary.pi_tool_snapshot(), before);
}

#[test]
fn pi_tool_snapshot_sibling_provenance_same_arc_override_stays_native() {
    let mut host = base_host();
    let original = read_tool(&host);
    let before = host.pi_tool_snapshot();
    let owner = host
        .dynamic_tools_with_authority(
            "same-arc-owner",
            vec![Arc::clone(&original)],
            BTreeSet::from(["read".into()]),
            None,
            |_, _| {},
        )
        .unwrap();
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
    assert!(host
        .dynamic_tools
        .read()
        .unwrap()
        .overridden_tools
        .contains_key("read"));
    let snapshot = host.pi_tool_snapshot();
    assert_eq!(pi_tool(&snapshot, "read")["sourceInfo"]["source"], "native");
    assert_eq!(
        pi_tool(&snapshot, "read")["sourceInfo"]["path"],
        "<native:read>"
    );
    owner.remove();
    assert_eq!(host.pi_tool_snapshot(), before);
}

#[test]
fn pi_tool_snapshot_filtered_builtin_same_arc_dynamic_republish_is_native() {
    let mut host = base_host();
    let original = read_tool(&host);
    host.retain_tools(|name| name != "read");
    let owner = host
        .dynamic_tools_with_authority(
            "republished-owner",
            vec![Arc::clone(&original)],
            BTreeSet::new(),
            None,
            |_, _| {},
        )
        .unwrap();
    assert!(!host
        .dynamic_tools
        .read()
        .unwrap()
        .overridden_tools
        .contains_key("read"));
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
    let snapshot = host.pi_tool_snapshot();
    assert_eq!(
        pi_tool(&snapshot, "read")["sourceInfo"],
        serde_json::json!({"source":"native","path":"<native:read>","scope":"temporary","origin":"top-level"})
    );
    owner.remove();
}

#[test]
fn pi_tool_snapshot_filtered_builtin_policy_same_arc_dynamic_republish_is_native() {
    let mut host = base_host();
    let original = read_tool(&host);
    let clone = host.clone();
    let before = clone.pi_tool_snapshot();
    host.set_tool_policy(|name| name != "read");
    assert!(!host.policed_tool_names().contains(&"read".into()));
    host.set_tool_policy(|_| true);
    assert!(!host.policed_tool_names().contains(&"read".into()));
    assert_eq!(clone.pi_tool_snapshot(), before);
    let owner = host
        .dynamic_tools_with_authority(
            "policy-republished-owner",
            vec![Arc::clone(&original)],
            BTreeSet::new(),
            None,
            |_, _| {},
        )
        .unwrap();
    assert!(!host
        .dynamic_tools
        .read()
        .unwrap()
        .overridden_tools
        .contains_key("read"));
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
    let snapshot = host.pi_tool_snapshot();
    assert_eq!(
        pi_tool(&snapshot, "read")["sourceInfo"],
        serde_json::json!({"source":"native","path":"<native:read>","scope":"temporary","origin":"top-level"})
    );
    owner.remove();
    assert!(!host.policed_tool_names().contains(&"read".into()));
    assert_eq!(clone.pi_tool_snapshot(), before);
}

#[test]
fn pi_tool_snapshot_filtered_builtin_different_arc_with_original_alive_is_native() {
    let mut host = base_host();
    let original = read_tool(&host);
    host.retain_tools(|name| name != "read");
    let replacement: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);
    assert!(!Arc::ptr_eq(&original, &replacement));
    let owner = host
        .dynamic_tools_with_authority(
            "different-arc-owner",
            vec![Arc::clone(&replacement)],
            BTreeSet::new(),
            None,
            |_, _| {},
        )
        .unwrap();
    assert!(!host
        .dynamic_tools
        .read()
        .unwrap()
        .overridden_tools
        .contains_key("read"));
    assert!(Arc::ptr_eq(&replacement, &read_tool(&host)));
    assert!(!Arc::ptr_eq(&original, &read_tool(&host)));
    let snapshot = host.pi_tool_snapshot();
    assert_eq!(
        pi_tool(&snapshot, "read")["sourceInfo"],
        serde_json::json!({"source":"native","path":"<native:read>","scope":"temporary","origin":"top-level"})
    );
    owner.remove();
}

#[test]
fn pi_tool_snapshot_filtered_builtin_dropped_original_dynamic_read_is_native() {
    let mut host = base_host();
    let original = read_tool(&host);
    let weak = Arc::downgrade(&original);
    host.retain_tools(|name| name != "read");
    drop(original);
    assert!(weak.upgrade().is_none());
    let replacement: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);
    let owner = host
        .dynamic_tools_with_authority(
            "dropped-original-owner",
            vec![Arc::clone(&replacement)],
            BTreeSet::new(),
            None,
            |_, _| {},
        )
        .unwrap();
    assert!(!host
        .dynamic_tools
        .read()
        .unwrap()
        .overridden_tools
        .contains_key("read"));
    assert!(Arc::ptr_eq(&replacement, &read_tool(&host)));
    let snapshot = host.pi_tool_snapshot();
    assert_eq!(
        pi_tool(&snapshot, "read")["sourceInfo"],
        serde_json::json!({"source":"native","path":"<native:read>","scope":"temporary","origin":"top-level"})
    );
    owner.remove();
}

#[test]
fn pi_tool_snapshot_builtin_provenance_uses_core_identity_not_names() {
    let mut ordinary = ExtensionHost::new();
    ordinary.tool(crate::tools::ReadTool);
    assert_eq!(
        pi_tool(&ordinary.pi_tool_snapshot(), "read")["sourceInfo"],
        serde_json::json!({"source":"native","path":"<native:read>","scope":"temporary","origin":"top-level"})
    );
    let mut host = base_host();
    let (revision, tools) = host.tool_snapshot();
    let definitions = serde_json::to_value(host.tool_definitions()).unwrap();
    let snapshot = host.pi_tool_snapshot();
    let names = tools
        .iter()
        .map(|tool| tool.definition().name)
        .collect::<Vec<_>>();
    assert_eq!(snapshot["active_tools"], serde_json::json!(names));
    assert_eq!(snapshot["all_tools"].as_array().unwrap().len(), tools.len());
    for tool in tools {
        let definition = tool.definition();
        let row = pi_tool(&snapshot, &definition.name);
        assert_eq!(row["parameters"], definition.parameters);
        assert_eq!(row["description"], definition.description);
        assert_eq!(
            row["promptGuidelines"],
            serde_json::json!(tool.prompt_metadata().map(|prompt| prompt.guidelines))
        );
        assert_eq!(
            row["sourceInfo"],
            serde_json::json!({"source":"builtin","path":format!("builtin:{}", definition.name),"scope":"temporary","origin":"top-level"})
        );
    }
    assert_eq!(host.pi_tool_snapshot(), snapshot);
    assert_eq!(host.tool_snapshot().0, revision);
    assert_eq!(
        serde_json::to_value(host.tool_definitions()).unwrap(),
        definitions
    );
    host.dynamic_tools("custom-owner", vec![Arc::new(Replacement("custom"))])
        .unwrap();
    assert_eq!(
        pi_tool(&host.pi_tool_snapshot(), "custom")["sourceInfo"],
        serde_json::json!({"source":"native","path":"<native:custom>","scope":"temporary","origin":"top-level"})
    );
}

#[test]
fn pi_tool_snapshot_builtin_provenance_tracks_override_and_restoration() {
    let mut host = base_host();
    host.set_active_tools(Some(&BTreeSet::from(["bash".into()])))
        .unwrap();
    let before = host.pi_tool_snapshot();
    assert_eq!(pi_tool(&before, "read")["sourceInfo"]["source"], "builtin");
    let definitions = serde_json::to_value(host.tool_definitions()).unwrap();
    let owner = publish(&mut host, "reviewed-owner", None);
    let effective = host.dynamic_tools.read().unwrap().overridden_tools["read"].clone();
    let overridden = host.pi_tool_snapshot();
    assert_eq!(overridden["active_tools"], serde_json::json!(["bash"]));
    assert_eq!(
        overridden["all_tools"].as_array().unwrap().len(),
        before["all_tools"].as_array().unwrap().len()
    );
    let row = pi_tool(&overridden, "read");
    assert_eq!(
        row["sourceInfo"],
        serde_json::json!({"source":"native","path":"<native:read>","scope":"temporary","origin":"top-level"})
    );
    assert_eq!(row["parameters"], effective.definition().parameters);
    assert_eq!(row["description"], "extension replacement");
    assert_eq!(
        row["promptGuidelines"],
        serde_json::json!(["Never use the displaced schema"])
    );
    assert_eq!(effective.replay_safety(), ReplaySafety::Unsafe);
    assert_eq!(effective.concurrency(), ToolConcurrency::Sequential);
    assert!(!effective.composition_is_unmetered());
    assert_eq!(
        serde_json::to_value(host.tool_definitions()).unwrap(),
        definitions
    );
    let revision = host.tool_snapshot().0;
    assert_eq!(host.pi_tool_snapshot(), overridden);
    assert_eq!(host.tool_snapshot().0, revision);
    owner.replace(vec![]).unwrap();
    assert_eq!(host.pi_tool_snapshot(), before);
    assert_eq!(effective.definition().description, "extension replacement");
    let newer = publish(&mut host, "new-owner", None);
    owner.remove();
    assert_eq!(
        pi_tool(&host.pi_tool_snapshot(), "read")["sourceInfo"]["source"],
        "native"
    );
    newer.remove();
    assert_eq!(host.pi_tool_snapshot(), before);
    assert_eq!(
        serde_json::to_value(host.tool_definitions()).unwrap(),
        definitions
    );
}

#[test]
fn builtin_override_needs_exact_owner_grant_and_real_builtin_identity() {
    let mut host = base_host();
    let original = read_tool(&host);
    assert!(host
        .dynamic_tools("unreviewed", vec![Arc::new(Replacement("read"))])
        .is_err());
    assert!(host
        .dynamic_tools_with_authority(
            "wrong-name",
            vec![Arc::new(Replacement("read"))],
            BTreeSet::from(["write".into()]),
            None,
            |_, _| {}
        )
        .is_err());
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));

    // Even the name of a standard builtin is not authority: only CoreTools can
    // mark the displaced implementation as a builtin. Arbitrary native tools,
    // first-party external tools and names reserved for later tools stay closed.
    for reserved in [false, true] {
        let mut host = ExtensionHost::new();
        if reserved {
            host.reserve_tool_names(["read"]);
        } else {
            host.tool(crate::tools::ReadTool);
        }
        assert!(host
            .dynamic_tools_with_authority(
                "reviewed",
                vec![Arc::new(Replacement("read"))],
                BTreeSet::from(["read".into()]),
                None,
                |_, _| {}
            )
            .is_err());
    }
    let mut host = base_host();
    host.dynamic_tools("first-party", vec![Arc::new(Replacement("external"))])
        .unwrap();
    assert!(host
        .dynamic_tools_with_authority(
            "reviewed",
            vec![Arc::new(Replacement("external"))],
            BTreeSet::from(["external".into()]),
            None,
            |_, _| {}
        )
        .is_err());
}

#[test]
fn builtin_override_all_effective_views_pin_replacement_not_displaced_privileges() {
    let mut host = base_host();
    let original = read_tool(&host);
    let clone = host.clone();
    publish(&mut host, "reviewed-instance", None);
    let effective = read_tool(&host);
    assert!(!Arc::ptr_eq(&original, &effective));
    assert!(Arc::ptr_eq(&effective, &read_tool(&clone)));
    assert_eq!(effective.replay_safety(), ReplaySafety::Unsafe);
    assert_eq!(effective.concurrency(), ToolConcurrency::Sequential);
    assert!(!effective.composition_is_unmetered());
    assert!(effective.output_schema().is_none());
    assert_eq!(original.replay_safety(), ReplaySafety::Safe);
    assert!(original.output_schema().is_some());

    let (_, snapshot) = host.model_tool_snapshot("owner");
    let direct = crate::tool_composition::direct_surface(&snapshot);
    assert_eq!(
        direct
            .iter()
            .filter(|tool| tool.definition().name == "read")
            .count(),
        1
    );
    assert!(Arc::ptr_eq(
        &effective,
        direct
            .iter()
            .find(|tool| tool.definition().name == "read")
            .unwrap()
    ));
    let nested = crate::tool_composition::callable_catalog(&snapshot);
    let nested_read = nested.iter().find(|tool| tool["name"] == "read").unwrap();
    assert_eq!(nested_read["parameters"], effective.definition().parameters);
    assert!(nested_read.get("output_schema").is_none());
    let pi = host.pi_tool_snapshot();
    let reads = pi["all_tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["name"] == "read")
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0]["description"], "extension replacement");
    assert_eq!(
        reads[0]["promptGuidelines"][0],
        "Never use the displaced schema"
    );
    let (child, names) = host
        .scoped_tool_snapshot(&BTreeSet::from(["read".into()]))
        .unwrap();
    assert_eq!(names, ["read"]);
    assert!(Arc::ptr_eq(&effective, &read_tool(&child)));
    assert_eq!(
        crate::tool::collect_tool_prompt_contributions(snapshot.iter().map(|tool| tool.as_ref()))
            [0]
        .snippet,
        "Use extension anchors"
    );
}

#[test]
fn builtin_override_replacement_reservation_and_conflict_are_atomic() {
    let mut host = base_host();
    let original = read_tool(&host);
    let owner = publish(&mut host, "reviewed-instance", None);
    let published = read_tool(&host);
    let before = host.tool_snapshot().0;
    assert!(host
        .dynamic_tools_with_authority(
            "other-instance",
            vec![Arc::new(Replacement("read"))],
            BTreeSet::from(["read".into()]),
            None,
            |_, _| {}
        )
        .is_err());
    assert!(owner.replace(vec![Arc::new(Replacement("write"))]).is_err());
    assert_eq!(host.tool_snapshot().0, before);
    assert!(Arc::ptr_eq(&published, &read_tool(&host)));
    let reservation = owner.reserve(vec![]).unwrap();
    assert!(Arc::ptr_eq(&published, &read_tool(&host)));
    drop(reservation); // failed speculative reload must not change the catalog
    assert!(Arc::ptr_eq(&published, &read_tool(&host)));
    owner.replace(vec![]).unwrap();
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
    assert_eq!(published.definition().description, "extension replacement");

    let owner = publish(&mut host, "reviewed-instance", None);
    owner.remove();
    let newer = publish(&mut host, "new-instance", None);
    owner.remove(); // retirement from an old process must never delete a new owner
    assert_eq!(
        read_tool(&host).definition().description,
        "extension replacement"
    );
    newer.remove();
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
}

#[test]
fn builtin_override_restoration_preserves_active_set_and_product_exclusions() {
    let mut host = base_host();
    host.set_active_tools(Some(&BTreeSet::from(["write".into()])))
        .unwrap();
    let owner = publish(&mut host, "reviewed", None);
    assert_eq!(
        host.tool_definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["write"]
    );
    owner.replace(vec![]).unwrap();
    assert_eq!(
        host.tool_definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["write"]
    );
    assert!(host.policed_tool_names().contains(&"read".into()));
    host.set_active_tools(Some(&BTreeSet::from(["read".into()])))
        .unwrap();
    assert_eq!(read_tool(&host).replay_safety(), ReplaySafety::Safe);
    publish(&mut host, "reviewed", None);
    host.set_tool_policy(|name| name != "read");
    owner.remove();
    assert!(!host.policed_tool_names().contains(&"read".into()));
    let pi = host.pi_tool_snapshot();
    assert!(pi["all_tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|tool| tool["name"] != "read"));
    assert_eq!(pi["active_tools"], serde_json::json!([]));
    assert!(host
        .set_active_tools(Some(&BTreeSet::from(["read".into()])))
        .is_err());
    assert!(owner
        .replace(vec![Arc::new(Replacement("read"))])
        .unwrap()
        .1
        .is_empty());
}

#[test]
fn builtin_override_closed_generation_restores_base_without_supervisor_or_retargeting() {
    let mut host = base_host();
    let original = read_tool(&host);
    let closed = Arc::new(AtomicBool::new(false));
    let owner = publish(&mut host, "reviewed", Some(Arc::clone(&closed)));
    let frozen = read_tool(&host);
    let before = host.tool_snapshot().0;
    // A candidate-first reload reserves new tools while the old process is live.
    let candidate_closed = Arc::new(AtomicBool::new(false));
    let candidate = owner
        .reserve_for_generation(
            vec![Arc::new(Replacement("read"))],
            Some(Arc::clone(&candidate_closed)),
        )
        .unwrap();
    assert_eq!(
        pi_tool(&host.pi_tool_snapshot(), "read")["sourceInfo"]["source"],
        "native"
    );
    closed.store(true, Ordering::Release);
    assert_eq!(
        pi_tool(&host.pi_tool_snapshot(), "read")["sourceInfo"],
        serde_json::json!({"source":"builtin","path":"builtin:read","scope":"temporary","origin":"top-level"})
    );
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
    assert!(host.tool_snapshot().0 > before);
    assert_eq!(frozen.definition().description, "extension replacement");
    candidate.commit().unwrap();
    let replacement = read_tool(&host);
    assert!(!Arc::ptr_eq(&frozen, &replacement));
    // The old generation's closed flag cannot retire the successful new one.
    assert!(Arc::ptr_eq(&replacement, &read_tool(&host)));
    assert_eq!(
        pi_tool(&host.pi_tool_snapshot(), "read")["sourceInfo"]["source"],
        "native"
    );
    candidate_closed.store(true, Ordering::Release);
    assert_eq!(
        pi_tool(&host.pi_tool_snapshot(), "read")["sourceInfo"]["path"],
        "builtin:read"
    );
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));

    let failed = Arc::new(AtomicBool::new(false));
    let candidate = owner
        .reserve_for_generation(
            vec![Arc::new(Replacement("read"))],
            Some(Arc::clone(&failed)),
        )
        .unwrap();
    failed.store(true, Ordering::Release);
    assert!(candidate.commit().is_err());
    assert!(Arc::ptr_eq(&original, &read_tool(&host)));
}

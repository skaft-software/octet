//! Native override negotiation, publication, policy and lifecycle regression gates.
use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn manifest(grants: &str) -> ExtensionManifest {
    ExtensionManifest::parse(&format!(
        r#"name = "reviewed-override"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "override.py"
[capabilities]
builtin_tool_overrides = [{grants}]
[contributes]
tools = ["read"]
"#
    ))
    .unwrap()
}

#[test]
fn builtin_override_manifest_is_bounded_exact_and_api_four_only() {
    let valid = manifest("\"read\"");
    assert_eq!(valid.capabilities.builtin_tool_overrides, ["read"]);
    for grants in [
        vec!["read", "read"],
        vec!["codemode"],
        vec!["Read"],
        vec!["read\n"],
    ] {
        let mut invalid = valid.clone();
        invalid.capabilities.builtin_tool_overrides =
            grants.into_iter().map(str::to_owned).collect();
        assert!(invalid.validate().is_err());
    }
    for version in ["0.1", "0.2", "0.3"] {
        let mut invalid = valid.clone();
        invalid.api_version = version.into();
        assert!(invalid.validate().is_err());
    }
    let empty = manifest("");
    assert!(serde_json::to_value(&empty.capabilities)
        .unwrap()
        .get("builtin_tool_overrides")
        .is_none());
}

fn initialize(with_feature: bool) -> InitializeResponse {
    let mut features = vec!["request_cancellation", "content_parts"];
    if with_feature {
        features.push("builtin_tool_overrides_v1");
    }
    serde_json::from_value(json!({
        "api_version":"0.4","tools":[{"name":"read","description":"replacement","parameters":{"type":"object"}}],
        "protocol":{"version":"0.4","features":features,"limits":{"max_concurrent_requests":1}}
    })).unwrap()
}

#[test]
fn builtin_override_negotiation_needs_declaration_and_explicit_selection() {
    for (grants, selected, accepted) in [
        ("", false, true),
        ("", true, false),
        ("\"read\"", false, false),
        ("\"read\"", true, true),
    ] {
        assert_eq!(
            negotiate_contributions_with_host_services(
                &manifest(grants),
                initialize(selected),
                1,
                OfferedHostServices::default()
            )
            .is_ok(),
            accepted
        );
    }
}

#[cfg(unix)]
const FIXTURE: &str = r#"#!/usr/bin/env python3
import json, pathlib, sys
send = lambda value: print(json.dumps(value), flush=True)
init = json.loads(sys.stdin.readline())
assert init['params']['capabilities']['builtin_tool_overrides'] == ['read']
assert 'builtin_tool_overrides_v1' in init['params']['protocol']['optional_features']
mode = pathlib.Path('mode').read_text() if pathlib.Path('mode').exists() else 'read'
name = 'write' if mode == 'conflict' else 'read'
tools = [] if mode == 'empty' else [{'name':name,'description':'replacement ' + mode,'parameters':{'type':'object'},'prompt_snippet':'extension anchors'}]
send({'jsonrpc':'2.0','id':init['id'],'result':{'api_version':'0.4','tools':tools,'protocol':{'version':'0.4','features':['request_cancellation','content_parts','dynamic_tools','builtin_tool_overrides_v1','tool_prompt_metadata_v1'],'limits':{'max_concurrent_requests':1}}}})
for line in sys.stdin:
    request = json.loads(line)
    if request.get('method') == 'tool/call':
        send({'jsonrpc':'2.0','id':request['id'],'result':{'content':[{'type':'text','text':'replacement ' + mode}]}})
        if request['params']['arguments'].get('unregister'):
            send({'jsonrpc':'2.0','id':'remove','method':'tools/unregister','params':{'names':['read']}})
    elif request.get('id') == 'remove':
        assert request['result'] == {'revision':1,'tools':[]}, request
        pathlib.Path('removed').write_text('ok')
    elif request.get('method') == 'shutdown':
        send({'jsonrpc':'2.0','id':request['id'],'result':{}})
        break
"#;

#[cfg(unix)]
async fn fixture() -> (TempDir, ExtensionProcess, ExtensionHost) {
    let temp = TempDir::new().unwrap();
    write_executable_script(&temp.path().join("override.py"), FIXTURE);
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    // The fleet manager disables per-process supervision; death must still
    // restore the base at the next catalog boundary without restarting first.
    config.supervise = false;
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest("\"read\"")),
        config,
    )
    .await
    .unwrap();
    let mut host = ExtensionHost::new();
    host.load(&crate::tools::CoreTools);
    process
        .try_register_dynamic_tool_catalog(&mut host)
        .unwrap();
    host.finalize_tool_surface();
    (temp, process, host)
}

#[cfg(unix)]
fn read_tool(host: &ExtensionHost) -> Arc<dyn Tool> {
    host.tool_snapshot()
        .1
        .into_iter()
        .find(|tool| tool.definition().name == "read")
        .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn builtin_override_real_process_retains_extension_effect_replay_and_policy() {
    let (temp, process, host) = fixture().await;
    let tool = read_tool(&host);
    let sandbox = crate::SandboxConfig::new(temp.path());
    let context = ToolContext {
        workspace: temp.path(),
        sandbox: &sandbox,
        execution_scope: "override-test",
        resource_owner: "owner",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: CancellationToken::default(),
    };
    assert_eq!(tool.replay_safety(), ReplaySafety::Unsafe);
    assert_eq!(tool.concurrency(), crate::ToolConcurrency::Sequential);
    assert!(!tool.composition_is_unmetered());
    assert_eq!(
        tool.effect(&json!({}), &context).unwrap(),
        ToolEffect::Extension
    );
    let intent = crate::effect::EffectIntent::new(
        "owner",
        "run",
        1,
        "call",
        "read",
        ToolEffect::Extension,
        json!({}),
    )
    .unwrap();
    assert!(crate::EffectBroker::default()
        .authorize(&intent, None)
        .await
        .is_err());
    assert_eq!(
        tool.execute(json!({}), &context).await.unwrap().text,
        "replacement read"
    );
    assert!(process.shutdown().await);
    assert_eq!(read_tool(&host).replay_safety(), ReplaySafety::Safe);
    assert!(
        tool.execute(json!({}), &context).await.is_err(),
        "a frozen call must never fall through to the builtin"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn builtin_override_real_unregister_reload_rollback_and_death_restore_base() {
    let (temp, process, host) = fixture().await;
    let frozen = read_tool(&host);
    std::fs::write(temp.path().join("mode"), "conflict").unwrap();
    assert!(process.reload().await.is_err());
    assert!(Arc::ptr_eq(&frozen, &read_tool(&host)));
    assert_eq!(
        process
            .call_tool("read", json!({}), process.current_context())
            .await
            .unwrap()
            .content,
        "replacement read"
    );
    std::fs::write(temp.path().join("mode"), "empty").unwrap();
    process.reload().await.unwrap();
    assert_eq!(read_tool(&host).replay_safety(), ReplaySafety::Safe);
    std::fs::write(temp.path().join("mode"), "read").unwrap();
    process.reload().await.unwrap();
    assert_eq!(read_tool(&host).replay_safety(), ReplaySafety::Unsafe);
    process
        .call_tool(
            "read",
            json!({"unregister":true}),
            process.current_context(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !temp.path().join("removed").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(read_tool(&host).replay_safety(), ReplaySafety::Safe);
    process.reload().await.unwrap();
    let connection = read_std_lock(&process.inner.connection).clone();
    connection.terminate().await;
    assert!(!process.is_running());
    assert_eq!(read_tool(&host).replay_safety(), ReplaySafety::Safe);
    process.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn builtin_override_same_source_new_instance_cannot_replace_or_remove_live_owner() {
    let (temp, process, mut host) = fixture().await;
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    let other = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest("\"read\"")),
        config,
    )
    .await
    .unwrap();
    let frozen = read_tool(&host);
    assert!(other.try_register_dynamic_tool_catalog(&mut host).is_err());
    assert!(Arc::ptr_eq(&frozen, &read_tool(&host)));
    process.detach_dynamic_tool_catalog();
    other.try_register_dynamic_tool_catalog(&mut host).unwrap();
    let replacement = read_tool(&host);
    assert!(!Arc::ptr_eq(&frozen, &replacement));
    process.shutdown().await;
    assert!(Arc::ptr_eq(&replacement, &read_tool(&host)));
    other.shutdown().await;
    assert_eq!(read_tool(&host).replay_safety(), ReplaySafety::Safe);
}

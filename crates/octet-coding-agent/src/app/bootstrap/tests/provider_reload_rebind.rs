use super::support::*;
use super::*;

const MODEL_ID: &str = "fixture-provider/fixture-model";

const PROVIDER: &str = r#"#!/usr/bin/env python3
import json
import os
import select
import sys
import time

variant = open(VARIANT_PATH).read().strip()


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def wait_for_retirement():
    import socket
    root = os.path.dirname(os.path.dirname(os.path.dirname(VARIANT_PATH)))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as gate:
        gate.settimeout(20)
        gate.connect(os.path.join(root, "retired.sock"))
        assert gate.recv(1) == b"r", "old provider retirement was not acknowledged"


def receive():
    line = sys.stdin.buffer.raw.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    if os.path.exists(VARIANT_PATH + ".preentry"):
        with open(VARIANT_PATH + ".protocol", "a") as trace:
            trace.write(canonical({"pid": os.getpid(), "direction": "receive", "value": value}) + "\n")
    with open(VARIANT_PATH + ".wire", "a") as trace:
        trace.write("%s %.3f %s\n" % (os.getpid(), time.monotonic(), value.get("method", value.get("id"))))
    return value


def send(value):
    if os.path.exists(VARIANT_PATH + ".preentry"):
        with open(VARIANT_PATH + ".protocol", "a") as trace:
            trace.write(canonical({"pid": os.getpid(), "direction": "send", "value": value}) + "\n")
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


initialize = receive()
api = "0.4" if variant.startswith("v04") else "0.3"
contract = initialize["params"].get("contract", {})
provider_capabilities = {"provider_catalog", "provider_stream", "provider_auth"}
provider_methods = {
    "providers/complete", "providers/register", "providers/update", "providers/unregister",
    "provider/stream", "provider/event", "provider/cancel", "provider/auth/request",
    "provider/auth/revoke",
}
selection = {} if api == "0.4" else {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": [
        c for c in contract["required_capabilities"] + contract["optional_capabilities"]
        if c in contract["required_capabilities"] or c in provider_capabilities
    ],
    "methods": [
        m for m in contract["required_methods"] + contract["optional_methods"]
        if m in contract["required_methods"] or m in provider_methods
    ],
    "limits": contract["limits"],
}
result = {"api_version": api, "tools": [], "contract": selection}
if api == "0.4":
    result = {"api_version": api, "tools": [], "commands": [], "protocol": {
        "version": api, "features": initialize["params"]["protocol"]["required_features"] + ["provider_proxy_v1"],
        "limits": {"max_concurrent_requests": 1}}}
send({"jsonrpc": "2.0", "id": initialize["id"], "result": result})
if variant == "failed":
    sys.exit(1)
model = {
    "id": "fixture-model" if variant != "withdrawn" else "other-model",
    "api_name": "fixture-model",
    "protocol": "openai_chat",
    "context_window": 16384 if variant == "limits" else 8192,
    "max_output_tokens": 1024,
    "capabilities": {"tools": False, "parallel_tool_calls": False,
                     "structured_output": False, "reasoning": False},
}
provider = {"id": "fixture-provider",
            "label": "relabelled provider" if variant == "label" else "fixture provider",
            "auth": {"kind": "none"}}
if api == "0.4":
    model["capabilities"]["tools"] = True
    model["pi_metadata"] = {"base_url": "http://127.0.0.1:1", "input": ["text"],
        "pricing": {"input": 0, "output": 0, "cache_read": 0, "cache_write_5m": 0,
                    "cache_write_1h": None, "reasoning": None, "tiers": []}}
if variant == "v04-url":
    model["pi_metadata"]["base_url"] += "/changed"
if variant == "v04-input":
    model["pi_metadata"]["input"] = ["text", "image"]
if variant == "v04-price":
    model["pi_metadata"]["pricing"]["input"] = 1
if variant == "v04-api":
    model["api_name"] = "changed-api"
if variant == "v04-unknown":
    model["pi_metadata"]["unknown"] = True
models = [model]
if variant == "catalog256":
    models += [dict(model, id="model-%03d" % i) for i in range(1, 256)]
if variant == "handoff-target":
    wait_for_retirement()
if variant != "none":
    send({"jsonrpc": "2.0", "id": "register", "method": "providers/register",
          "params": {"provider": provider, "models": models}})
    response = receive()
    assert response.get("id") == "register" and "result" in response, response
if variant == "partial":
    sys.exit(1)
send({"jsonrpc": "2.0", "method": "providers/complete", "params": {}})
updated = False
original_models = json.loads(json.dumps(models))
hook_update_sequence = 0
rpc_listener = None
rpc_sequence = 0
if os.path.exists(VARIANT_PATH + ".rpc"):
    import socket
    rpc_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    rpc_listener.bind(VARIANT_PATH + ".control")
    rpc_listener.listen(1)
while True:
    if variant == "catalog256" and not updated and os.path.exists(VARIANT_PATH + ".update"):
        models[-1]["display_name"] = "unselected model updated"
        send({"jsonrpc": "2.0", "id": "update", "method": "providers/update",
              "params": {"provider": provider, "models": models}})
        response = receive()
        assert response.get("id") == "update" and "result" in response, response
        open(VARIANT_PATH + ".updated", "w").write("ready")
        updated = True
    readable = select.select([sys.stdin] + ([rpc_listener] if rpc_listener else []), [], [], 0.01)[0]
    if rpc_listener in readable:
        control, _ = rpc_listener.accept()
        with control, control.makefile("rwb", buffering=0) as channel:
            command = json.loads(channel.readline())
            mutation = command["mutation"]
            rpc_sequence += 1
            request_id = "rpc-update-%s" % rpc_sequence
            if mutation in ("revoke", "ready"):
                action = "revoke" if mutation == "revoke" else "authorize"
                method = "provider/auth/revoke" if mutation == "revoke" else "provider/auth/request"
                params = {"provider_id": provider["id"], "action": action, "interactive": False}
            else:
                if mutation == "selected":
                    models[0]["context_window"] += 1
                elif mutation == "restore":
                    models = json.loads(json.dumps(original_models))
                elif mutation == "unrelated":
                    models.append(dict(models[0], id="other-model"))
                else:
                    assert mutation == "identical", mutation
                method, params = "providers/update", {"provider": provider, "models": models}
            send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
            response = receive()
            assert response.get("id") == request_id and "result" in response, response
            if mutation in ("revoke", "ready"):
                assert response["result"]["status"] == ("revoked" if mutation == "revoke" else "ready"), response
            acknowledgment = {"command": command, "host_response": response, "pid": os.getpid()}
            with open(VARIANT_PATH + ".control-log", "a") as trace:
                trace.write(canonical(acknowledgment) + "\n")
            channel.write((canonical(acknowledgment) + "\n").encode())
    if sys.stdin not in readable:
        continue
    message = receive()
    if message.get("method") == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
    if message.get("method") == "hook/run":
        if os.path.exists(VARIANT_PATH + ".hook"):
            mutation = open(VARIANT_PATH + ".hook").read().strip()
            if mutation in ("preentry-revoke", "preentry-ready"):
                action = "revoke" if mutation == "preentry-revoke" else "authorize"
                method = "provider/auth/revoke" if action == "revoke" else "provider/auth/request"
                params = {"provider_id": provider["id"], "action": action, "interactive": False}
            elif mutation == "withdraw":
                method, params = "providers/unregister", {"provider_id": provider["id"]}
            else:
                if mutation == "base_url":
                    models[0]["pi_metadata"]["base_url"] += "/changed"
                elif mutation == "input":
                    models[0]["pi_metadata"]["input"] = ["text", "image"]
                elif mutation == "pricing":
                    models[0]["pi_metadata"]["pricing"]["input"] += 1
                elif mutation == "api_name":
                    models[0]["api_name"] += "-changed"
                elif mutation == "label":
                    provider["label"] += " changed"
                elif mutation == "preentry-restore":
                    models = json.loads(json.dumps(original_models))
                elif mutation == "preentry-identical":
                    pass
                elif mutation == "unrelated":
                    other = json.loads(json.dumps(models[0]))
                    other["id"] = "other-model"
                    models.append(other)
                else:
                    models[0]["context_window"] += 1
                method, params = "providers/update", {"provider": provider, "models": models}
            hook_update_sequence += 1
            request_id = "hook-update-%s" % hook_update_sequence if os.path.exists(VARIANT_PATH + ".preentry") else "hook-update"
            send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
            response = receive()
            assert response.get("id") == request_id and "result" in response, response
            if mutation in ("preentry-revoke", "preentry-ready"):
                assert response["result"]["status"] == ("revoked" if mutation == "preentry-revoke" else "ready"), response
            open(VARIANT_PATH + ".hooked", "w").write(mutation)
        send({"jsonrpc": "2.0", "id": message["id"], "result": {
            "disposition": {"action": "continue"}, "context": [], "notifications": []}})
    if message.get("method") == "provider/stream":
        stream_id = message["params"]["stream_id"]
        send({"jsonrpc": "2.0", "id": message["id"],
              "result": {"stream_id": stream_id, "accepted": True}})
        text = "LOCAL-PROVIDER-%s" % os.getpid()
        with open(VARIANT_PATH + ".streams", "a") as trace:
            trace.write(text + "\n")
        events = [("started", {"response_id": stream_id}), ("text_start", {"index": 0}),
                  ("text_delta", {"index": 0, "delta": text}), ("text_end", {"index": 0}),
                  ("finished", {"stop_reason": "stop"})]
        if variant == "held":
            events = events[:3]
        for sequence, (kind, payload) in enumerate(events):
            send({"jsonrpc": "2.0", "method": "provider/event", "params": {
                "stream_id": stream_id, "sequence": sequence, "kind": kind, "payload": payload}})
if rpc_listener:
    rpc_listener.close()
    os.unlink(VARIANT_PATH + ".control")
"#;

fn write_provider(root: &Path, name: &str, variant: &str) {
    let provider = root.join(name);
    std::fs::create_dir_all(&provider).unwrap();
    std::fs::write(
        provider.join("provider.py"),
        PROVIDER.replace(
            "VARIANT_PATH",
            &format!("{:?}", provider.join("variant").display().to_string()),
        ),
    )
    .unwrap();
    std::fs::write(provider.join("variant"), variant).unwrap();
    let preentry = root.parent().unwrap().join("preentry-evidence").exists();
    if preentry {
        std::fs::write(provider.join("variant.preentry"), "enabled").unwrap();
        if root.parent().unwrap().join("rpc-evidence").exists() {
            std::fs::write(provider.join("variant.rpc"), "enabled").unwrap();
        }
        std::fs::write(
            provider.join("reap.py"),
            r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import subprocess
import sys
root = Path(FIXTURE_ROOT)
with (root / "variant.environment").open("a") as trace:
    trace.write(json.dumps({"pid": os.getpid(), "environment": dict(os.environ)}) + "\n")
child = subprocess.Popen([sys.executable, str(root / "provider.py")])
with (root / "variant.pids").open("a") as trace:
    trace.write(json.dumps({"reaper": os.getpid(), "fixture": child.pid}) + "\n")
code = child.wait()
with (root / "variant.exits").open("a") as trace:
    trace.write(json.dumps({"reaper": os.getpid(), "fixture": child.pid, "exit": code}) + "\n")
sys.exit(code)
"#
            .replace(
                "FIXTURE_ROOT",
                &format!("{:?}", provider.display().to_string()),
            ),
        )
        .unwrap();
    }
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(
        provider.join("provider.py"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    if preentry {
        std::fs::set_permissions(
            provider.join("reap.py"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let command = if preentry { "reap.py" } else { "provider.py" };
    let (api, hooks) = if variant.starts_with("v04") {
        ("0.4", "hooks = [\"before_prompt\"]\n")
    } else {
        ("0.3", "")
    };
    std::fs::write(
        provider.join("extension.toml"),
        format!(
            "name = \"{name}\"\nversion = \"0.3.0\"\napi_version = \"{api}\"\n\n[entrypoint]\ncommand = \"{command}\"\n\n[contributes]\nproviders = true\n{hooks}"
        ),
    )
    .unwrap();
}

pub(crate) fn provider_app(directory: &Path) -> App {
    provider_app_with(directory, &[("rebind-provider", "same")])
}

pub(crate) fn provider_hook_app(directory: &Path) -> App {
    provider_app_with(directory, &[("rebind-provider", "v04")])
}

fn provider_app_with(directory: &Path, extensions: &[(&str, &str)]) -> App {
    let root = directory.join("extensions");
    for (name, variant) in extensions {
        write_provider(&root, name, variant);
    }
    let names = extensions
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<Vec<_>>();
    let v04 = extensions
        .iter()
        .any(|(_, variant)| variant.starts_with("v04"));
    let mut config = config(
        directory,
        Some(if v04 {
            "gpt-5.4-mini-responses"
        } else {
            MODEL_ID
        }),
    );
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.extension_paths = vec![root];
    config.enabled_extensions = names.clone();
    config.invocation_trusted_extensions = names;
    let boot = bootstrap(config).unwrap();
    boot.preflight_extension_providers().unwrap();
    let launch = resolve_launch_print(&boot, "provider-rebind").unwrap();
    let mut app = build_app(boot, launch, "system".into()).unwrap();
    if v04 {
        app.synchronize_extension_provider_catalog();
        let model = app.catalog.resolve(&ModelId(MODEL_ID.into())).unwrap();
        let reasoning = default_reasoning_for_model(&model);
        assert!(try_select_model_at_idle(&mut app, model, Some(reasoning)).unwrap());
    }
    assert_eq!(app.model.spec.id.0, MODEL_ID);
    app
}

pub(crate) fn set_variant(directory: &Path, variant: &str) {
    set_extension_variant(directory, "rebind-provider", variant);
}

fn set_extension_variant(directory: &Path, name: &str, variant: &str) {
    std::fs::write(
        directory.join("extensions").join(name).join("variant"),
        variant,
    )
    .unwrap();
}

async fn reload_in_place(app: &mut App) {
    let reloads = app.executable_extensions.reload().await;
    assert!(
        reloads
            .iter()
            .any(|message| message.starts_with("reloaded rebind-provider (generation ")),
        "provider reload failed: {reloads:?}"
    );
}

fn assert_bound_to_live_route(app: &App) {
    let live = app.catalog.resolve(&ModelId(MODEL_ID.into())).unwrap();
    assert_eq!(app.model.endpoint.id, live.endpoint.id);
    assert_eq!(app.agent.model().endpoint.id, live.endpoint.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_reregistration_after_process_reload_rebinds_the_active_model() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app(directory.path());
    let first = app.model.endpoint.id.clone();

    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog_for_request()
        .unwrap();
    assert_ne!(app.model.endpoint.id, first);
    assert_bound_to_live_route(&app);

    let second = app.model.endpoint.id.clone();
    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog_for_request()
        .unwrap();
    assert_ne!(app.model.endpoint.id, second);
    assert_bound_to_live_route(&app);

    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_repeated_before_prompt_preserves_authorized_continuity() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app(directory.path());
    let first = app.model.endpoint.id.clone();
    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog();
    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog_for_request()
        .unwrap();
    assert_ne!(app.model.endpoint.id, first);
    assert_bound_to_live_route(&app);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_selection_does_not_cross_new_resume_or_fork_sessions() {
    let mut transferred = Vec::new();
    for transition in ["new", "resume", "fork", "same-session-resume"] {
        let directory = tempfile::tempdir().unwrap();
        let app = provider_app(directory.path());
        let first = app.model.endpoint.id.clone();
        let current = app.agent.session().path().to_owned();
        let selection = match transition {
            "new" => SessionSelection::CreateNew(app.sessions.new_path("new-provider")),
            "resume" => {
                let path = app.sessions.new_path("resume-provider");
                drop(Session::create(&path).unwrap());
                SessionSelection::OpenExisting(path)
            }
            "fork" => {
                let destination = app.sessions.new_path("fork-provider");
                let path = fork_session_into(&app.sessions, &current, destination).unwrap();
                SessionSelection::Forked(path)
            }
            _ => SessionSelection::OpenExisting(current),
        };
        let mut app = rebuild_app(app, None, None, None, Some(selection)).unwrap();
        let result = app.synchronize_extension_provider_catalog_for_request();
        if result.is_ok() {
            transferred.push(transition);
        } else {
            assert_eq!(app.model.endpoint.id, first, "{transition}");
            assert_eq!(app.agent.model().endpoint.id, first, "{transition}");
        }
        app.executable_extensions.shutdown().await;
    }
    assert!(
        transferred.is_empty(),
        "retired selection transferred on {transferred:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_reregistration_after_full_reload_rebinds_the_active_model() {
    let directory = tempfile::tempdir().unwrap();
    let app = provider_app(directory.path());
    let first = app.model.endpoint.id.clone();

    let mut app = rebuild_app_for_reload(app).unwrap();
    assert_eq!(app.model.endpoint.id, first);
    app.synchronize_extension_provider_catalog_for_request()
        .unwrap();
    assert_ne!(app.model.endpoint.id, first);
    assert_bound_to_live_route(&app);

    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_or_withdrawn_reregistration_keeps_the_route_fence() {
    for (variant, expected) in [
        ("limits", "route changed"),
        ("label", "route changed"),
        ("withdrawn", "no longer available"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut app = provider_app(directory.path());
        let first = app.model.endpoint.id.clone();
        set_variant(directory.path(), variant);
        reload_in_place(&mut app).await;
        let error = app
            .synchronize_extension_provider_catalog_for_request()
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{variant}: {error}");
        assert_eq!(app.model.endpoint.id, first, "{variant}");
        assert_eq!(app.agent.model().endpoint.id, first, "{variant}");
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_failed_or_partial_registration_cannot_rebind() {
    for variant in ["failed", "partial"] {
        let directory = tempfile::tempdir().unwrap();
        let mut app = provider_app(directory.path());
        let old = app.model.endpoint.id.clone();
        set_variant(directory.path(), variant);
        let _ = app.executable_extensions.reload().await;
        assert!(app
            .synchronize_extension_provider_catalog_for_request()
            .is_err());
        assert_eq!(app.model.endpoint.id, old);
        assert_eq!(app.agent.model().endpoint.id, old);
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_without_host_receipt_cannot_rebind() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app(directory.path());
    let old = app.model.endpoint.id.clone();
    let manager = app.executable_extensions.runtime_manager().unwrap();
    let outcomes = manager.reload("rebind-provider").await;
    assert!(outcomes.iter().all(Result::is_ok));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            app.synchronize_extension_provider_catalog();
            if app
                .catalog
                .resolve(&ModelId(MODEL_ID.into()))
                .is_ok_and(|model| model.endpoint.id != old)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app
        .synchronize_extension_provider_catalog_for_request()
        .is_err());
    assert_eq!(app.model.endpoint.id, old);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_later_manager_generation_cannot_reuse_authorized_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app(directory.path());
    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog();
    let authorized = app
        .catalog
        .resolve(&ModelId(MODEL_ID.into()))
        .unwrap()
        .endpoint
        .id
        .clone();
    let outcomes = app
        .executable_extensions
        .runtime_manager()
        .unwrap()
        .reload("rebind-provider")
        .await;
    assert!(outcomes.iter().all(Result::is_ok));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            app.synchronize_extension_provider_catalog();
            if app
                .catalog
                .resolve(&ModelId(MODEL_ID.into()))
                .is_ok_and(|model| model.endpoint.id != authorized)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app
        .synchronize_extension_provider_catalog_for_request()
        .is_err());
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_metadata_changes_and_unknown_fields_refuse() {
    for full in [false, true] {
        for variant in [
            "v04-url",
            "v04-input",
            "v04-price",
            "v04-api",
            "v04-unknown",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut app = provider_hook_app(directory.path());
            let old = app.model.endpoint.id.clone();
            set_variant(directory.path(), variant);
            if full {
                app = rebuild_app_for_reload(app).unwrap();
            } else {
                let _ = app.executable_extensions.reload().await;
            }
            assert!(
                app.synchronize_extension_provider_catalog_for_request()
                    .is_err(),
                "full={full}, {variant}"
            );
            assert_eq!(app.model.endpoint.id, old);
            app.executable_extensions.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_cancelled_prepare_or_unfinished_replacement_cannot_authorize() {
    for replace in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut app = provider_app(directory.path());
        let old = app.model.endpoint.id.clone();
        let work = app.executable_extensions.prepare_resource_process_reload();
        if replace {
            let outcomes = work.await;
            assert!(outcomes.iter().all(|(_, result)| result.is_ok()));
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    app.synchronize_extension_provider_catalog();
                    if app
                        .catalog
                        .resolve(&ModelId(MODEL_ID.into()))
                        .is_ok_and(|model| model.endpoint.id != old)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(app
                .synchronize_extension_provider_catalog_for_request()
                .is_err());
        } else {
            drop(work);
            app.synchronize_extension_provider_catalog_for_request()
                .unwrap();
        }
        assert_eq!(app.model.endpoint.id, old);
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_selection_consumes_receipt_and_failed_selection_does_not_mint_one() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app(directory.path());
    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog();
    let current = app.catalog.resolve(&ModelId(MODEL_ID.into())).unwrap();
    let incompatible = app
        .catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    assert!(!try_select_model_at_idle(&mut app, incompatible.clone(), None).unwrap());
    assert!(app
        .executable_extensions
        .reloaded_provider_model(&app.catalog, &app.model)
        .is_some());
    assert!(try_select_model_at_idle(&mut app, current, None).unwrap());
    assert!(app
        .executable_extensions
        .reloaded_provider_model(&app.catalog, &app.model)
        .is_none());
    let outcomes = app
        .executable_extensions
        .runtime_manager()
        .unwrap()
        .reload("rebind-provider")
        .await;
    assert!(outcomes.iter().all(Result::is_ok));
    assert!(!try_select_model_at_idle(&mut app, incompatible, None).unwrap());
    assert!(app
        .synchronize_extension_provider_catalog_for_request()
        .is_err());
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_held_old_generation_stream_terminates_once_and_replacement_streams() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app_with(directory.path(), &[("rebind-provider", "held")]);
    let mut run = app.agent.prompt("held old generation").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match run.next().await.unwrap() {
                octet_agent::AgentEvent::OutputDelta { .. } => break,
                octet_agent::AgentEvent::RunFinished { .. } => {
                    panic!("held stream ended before reload")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    set_variant(directory.path(), "same");
    let reloads = app.executable_extensions.reload().await;
    assert!(reloads
        .iter()
        .any(|line| line.starts_with("reloaded rebind-provider")));
    let mut terminals = 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = run.next().await {
            if let octet_agent::AgentEvent::RunFinished { reason, .. } = event {
                assert!(matches!(reason, octet_agent::FinishReason::Failed(_)));
                terminals += 1;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(terminals, 1);
    drop(run);
    app.synchronize_extension_provider_catalog_for_request()
        .unwrap();
    let output = app.agent.complete("replacement only").await.unwrap();
    assert!(output.text.starts_with("LOCAL-PROVIDER-"));
    let streams = std::fs::read_to_string(
        directory
            .path()
            .join("extensions/rebind-provider/variant.streams"),
    )
    .unwrap();
    let calls = streams.lines().collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0], calls[1]);
    assert_eq!(output.text, calls[1]);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_changed_selected_facts_before_capture_cannot_authorize() {
    for pending in [false, true] {
        for mutation in [
            "update",
            "withdraw",
            "base_url",
            "input",
            "pricing",
            "api_name",
            "label",
            "generation",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut app = provider_hook_app(directory.path());
            if pending {
                reload_in_place(&mut app).await;
                app.synchronize_extension_provider_catalog();
            }
            let runtime = app.executable_extensions.provider_runtime();
            let before = runtime
                .registered_route_for_test(&app.model.spec.id)
                .unwrap();
            if mutation == "generation" {
                let outcomes = app
                    .executable_extensions
                    .runtime_manager()
                    .unwrap()
                    .reload("rebind-provider")
                    .await;
                assert!(outcomes.iter().all(Result::is_ok));
                tokio::time::timeout(Duration::from_secs(5), async {
                    while runtime
                        .registered_route_for_test(&app.model.spec.id)
                        .is_none_or(|route| route.0.owner == before.0.owner)
                    {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
            } else {
                let prefix = directory.path().join("extensions/rebind-provider/variant");
                std::fs::write(prefix.with_extension("hook"), mutation).unwrap();
                app.executable_extensions
                    .compose_prompt(
                        &app.system,
                        "change selected declaration before reload capture".into(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    std::fs::read_to_string(prefix.with_extension("hooked")).unwrap(),
                    mutation
                );
            }
            reload_in_place(&mut app).await;
            assert!(
                app.synchronize_extension_provider_catalog_for_request()
                    .is_err(),
                "prior receipt={pending}, mutation={mutation}"
            );
            app.executable_extensions.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_unselected_update_before_receipt_preparation_preserves_continuity() {
    let mut lost = Vec::new();
    for pending in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut app = provider_app_with(directory.path(), &[("rebind-provider", "catalog256")]);
        if pending {
            reload_in_place(&mut app).await;
            app.synchronize_extension_provider_catalog();
        }
        let runtime = app.executable_extensions.provider_runtime();
        let before = runtime
            .registered_route_for_test(&app.model.spec.id)
            .unwrap();
        let prefix = directory.path().join("extensions/rebind-provider/variant");
        std::fs::write(prefix.with_extension("update"), "update").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !prefix.with_extension("updated").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let changed = runtime
            .registered_route_for_test(&app.model.spec.id)
            .unwrap();
        assert_ne!(
            changed.0.registration_revision,
            before.0.registration_revision
        );
        assert_eq!(changed.1, before.1);
        reload_in_place(&mut app).await;
        if app
            .synchronize_extension_provider_catalog_for_request()
            .is_err()
        {
            lost.push(pending);
        } else {
            assert_bound_to_live_route(&app);
        }
        app.executable_extensions.shutdown().await;
    }
    assert!(
        lost.is_empty(),
        "unselected update before capture lost continuity, prior receipt={lost:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_unrelated_update_after_projection_keeps_selected_authority() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app_with(directory.path(), &[("rebind-provider", "catalog256")]);
    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog();
    let prefix = directory.path().join("extensions/rebind-provider/variant");
    std::fs::write(prefix.with_extension("update"), "update").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !prefix.with_extension("updated").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app
        .executable_extensions
        .reloaded_provider_model(&app.catalog, &app.model)
        .is_some());
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_reload_continuity_survives_a_full_catalog_update_before_admission() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = provider_app_with(directory.path(), &[("rebind-provider", "catalog256")]);
    let old = app.model.endpoint.id.clone();
    reload_in_place(&mut app).await;
    app.synchronize_extension_provider_catalog();
    let signal = directory
        .path()
        .join("extensions/rebind-provider/variant.update");
    std::fs::write(&signal, "update").unwrap();
    let ready = directory
        .path()
        .join("extensions/rebind-provider/variant.updated");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scripted catalog update did not settle");
    app.synchronize_extension_provider_catalog_for_request()
        .unwrap();
    assert_ne!(app.model.endpoint.id, old);
    assert_bound_to_live_route(&app);
    app.executable_extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_identity_from_another_extension_is_not_rebound() {
    use tokio::io::AsyncWriteExt as _;

    let directory = tempfile::tempdir().unwrap();
    let gate = tokio::net::UnixListener::bind(directory.path().join("retired.sock")).unwrap();
    let mut app = provider_app_with(
        directory.path(),
        &[("rebind-provider", "same"), ("rebind-intruder", "none")],
    );
    let first = app.model.endpoint.id.clone();
    for name in ["rebind-provider", "rebind-intruder"] {
        std::fs::write(
            directory
                .path()
                .join("extensions")
                .join(name)
                .join("variant.preentry"),
            "enabled",
        )
        .unwrap();
    }
    set_extension_variant(directory.path(), "rebind-provider", "none");
    set_extension_variant(directory.path(), "rebind-intruder", "handoff-target");
    // Capture the selected route and reload both processes concurrently, but
    // hold the foreign registration until all old generations have retired.
    // Finish this same reload only after the foreign route is live, so the
    // continuity decision observes the competing extension's actual owner.
    let pending = app.executable_extensions.prepare_resource_process_reload();
    let (results, mut target) = tokio::time::timeout(Duration::from_secs(15), async {
        let (results, connection) = tokio::join!(pending, gate.accept());
        (results, connection.unwrap().0)
    })
    .await
    .expect("provider reload and foreign registration gate did not settle");
    let runtime = app.executable_extensions.provider_runtime();
    assert!(
        runtime
            .registered_route_for_test(&app.model.spec.id)
            .is_none(),
        "old provider route still exists after the reload processes settled: {results:?}"
    );
    target.write_all(b"r").await.unwrap();
    let intruder = app
        .executable_extensions
        .test_process("rebind-intruder")
        .unwrap();
    let foreign = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            // Registry resolution only exposes completed initial catalogs.
            if let Some((route, _)) = runtime.registered_route_for_test(&app.model.spec.id) {
                if route.owner.extension_instance_id == intruder.extension_instance_id()
                    && route.owner.generation == intruder.health_snapshot().generation
                {
                    break route;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let traces = ["rebind-provider", "rebind-intruder"].map(|name| {
            let path = directory.path().join("extensions").join(name).join("variant.protocol");
            (name, std::fs::read_to_string(path).unwrap_or_default())
        });
        panic!("intruder did not publish a completed live route: {results:?}; health: {:?}; traces: {traces:?}", intruder.health_snapshot());
    });
    let reload = app
        .executable_extensions
        .finish_resource_process_reload(results)
        .await;
    for name in ["rebind-provider", "rebind-intruder"] {
        assert!(
            reload
                .processes
                .iter()
                .any(|(extension, result)| extension == name && result.is_ok()),
            "{name} reload failed: {reload:?}"
        );
    }
    assert!(intruder.is_running());
    assert_eq!(
        foreign.owner.extension_instance_id,
        intruder.extension_instance_id()
    );
    let error = app
        .synchronize_extension_provider_catalog_for_request()
        .unwrap_err();
    assert!(error.to_string().contains("route changed"), "{error}");
    assert_ne!(
        app.catalog
            .resolve(&ModelId(MODEL_ID.into()))
            .unwrap()
            .endpoint
            .id,
        first
    );
    assert_eq!(app.model.endpoint.id, first);
    assert_eq!(app.agent.model().endpoint.id, first);
    app.executable_extensions.shutdown().await;
}

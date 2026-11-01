use super::*;
use pretty_assertions::assert_eq;

const PROVIDER: &str = r#"#!/usr/bin/env python3
import json
import os
import select
import sys
import time
import traceback


def trace(value):
    with open(PREFIX + ".trace", "a") as ledger:
        ledger.write(json.dumps({"case": CASE_LABEL, "pid": os.getpid(), "time": time.monotonic(), "event": value}, sort_keys=True) + "\n")


def send(value):
    trace({"send": value})
    print(json.dumps(value, separators=(",", ":"), sort_keys=True), flush=True)


def receive():
    line = sys.stdin.buffer.raw.readline()
    assert line
    value = json.loads(line)
    trace({"receive": value})
    return value


def request(identifier, method, params):
    send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
    response = receive()
    assert response.get("id") == identifier and "result" in response, response


def failed(kind, value, stack):
    with open(PREFIX + ".stderr", "a") as ledger:
        traceback.print_exception(kind, value, stack, file=ledger)
    sys.__excepthook__(kind, value, stack)


sys.excepthook = failed
allowed_environment = {"PATH", "HOME", "TMPDIR", "LANG", "LC_CTYPE", "PYTHONDONTWRITEBYTECODE", "OCTET_EXTENSION_API_VERSION", "OCTET_EXTENSION_NAME", "OCTET_EXTENSION_DIR", "OCTET_EXTENSION_MANIFEST", "OCTET_WORKSPACE", "OCTET_EXTENSION_SCRATCH"}
trace({"environment_names": sorted(os.environ)})
trace({"environment": {name: os.environ[name] for name in allowed_environment if name in os.environ}})
platform_environment = {"__CF_USER_TEXT_ENCODING"} if sys.platform == "darwin" else set()
assert set(os.environ) <= allowed_environment | platform_environment, sorted(set(os.environ) - allowed_environment - platform_environment)
initialize = receive()
contract = initialize["params"]["contract"]
capabilities = {"provider_catalog", "provider_stream", "provider_auth"}
methods = {"providers/complete", "providers/register", "providers/update", "providers/unregister", "provider/stream", "provider/event", "provider/cancel", "provider/auth/request", "provider/auth/revoke"}
selection = dict(contract)
selection["capabilities"] = contract["required_capabilities"] + [c for c in contract["optional_capabilities"] if c in capabilities]
selection["methods"] = contract["required_methods"] + [m for m in contract["optional_methods"] if m in methods]
for key in ["required_capabilities", "optional_capabilities", "required_methods", "optional_methods"]:
    del selection[key]
send({"jsonrpc": "2.0", "id": initialize["id"], "result": {"api_version": "0.3", "tools": [], "contract": selection}})
provider = {"id": "queue-provider", "label": "Queue provider", "auth": {"kind": "none"}}
model = {"id": "selected", "api_name": "selected", "protocol": "openai_chat", "context_window": 8192, "max_output_tokens": 1024, "capabilities": {"tools": False, "parallel_tool_calls": False, "structured_output": False, "reasoning": False}}
models = [model, dict(model, id="unselected")]
request("register", "providers/register", {"provider": provider, "models": models})
send({"jsonrpc": "2.0", "method": "providers/complete", "params": {}})
mutated = False
pending_stream = None
while True:
    if pending_stream is not None and os.path.exists(PREFIX + ".release"):
        trace({"release": pending_stream})
        events = [("started", {"response_id": pending_stream}), ("text_start", {"index": 0}), ("text_delta", {"index": 0, "delta": "queue-ok"}), ("text_end", {"index": 0}), ("finished", {"stop_reason": "stop"})]
        for sequence, (kind, payload) in enumerate(events):
            send({"jsonrpc": "2.0", "method": "provider/event", "params": {"stream_id": pending_stream, "sequence": sequence, "kind": kind, "payload": payload}})
        pending_stream = None
    if not mutated and os.path.exists(PREFIX + ".mutate"):
        mutation = open(PREFIX + ".mutate").read()
        if mutation == "withdraw":
            request("mutate", "providers/unregister", {"provider_id": provider["id"]})
        else:
            if mutation == "selected":
                models[0]["context_window"] += 1
            elif mutation == "unrelated":
                models[1]["display_name"] = "unselected changed"
            request("mutate", "providers/update", {"provider": provider, "models": models})
        open(PREFIX + ".mutated", "w").write(mutation)
        mutated = True
    if not select.select([sys.stdin], [], [], 0.005)[0]:
        continue
    message = receive()
    method = message.get("method")
    if method == "provider/stream":
        with open(PREFIX + ".streams", "a") as ledger:
            ledger.write(str(os.getpid()) + "\n")
        stream = message["params"]["stream_id"]
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"stream_id": stream, "accepted": True}})
        pending_stream = stream
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
    elif method == "provider/cancel":
        if pending_stream == message["params"]["stream_id"]:
            pending_stream = None
    elif method != "$/cancelRequest":
        raise AssertionError(message)
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_provider_admission_rechecks_after_capacity_wait() {
    let mut violations = Vec::new();
    for capacity in ["slots", "writer"] {
        for mutation in [
            "none",
            "unrelated",
            "selected",
            "withdraw",
            "revoke",
            "generation",
            "selected-restored",
            "revoke-restored",
            "withdraw-restored",
            "cancel",
            "timeout",
        ] {
            eprintln!("CASE START capacity={capacity} mutation={mutation}");
            let (accepted, callback, completed) = queued_case(capacity, mutation).await;
            eprintln!("CASE RESULT capacity={capacity} mutation={mutation} accepted={accepted} callback={callback} completed={completed}");
            let valid = if matches!(mutation, "none" | "unrelated") {
                accepted && callback && completed
            } else {
                !accepted && !callback
            };
            if !valid {
                violations.push((capacity, mutation, accepted, callback, completed));
            }
        }
    }
    assert!(violations.is_empty(), "queued admission violations (capacity, mutation, accepted, callback, completed): {violations:?}");
}

async fn queued_case(capacity: &str, mutation: &str) -> (bool, bool, bool) {
    let label = format!("capacity={capacity} mutation={mutation}");
    let directory = TempDir::new().unwrap().keep();
    let prefix = directory.join("queue");
    eprintln!("CASE ARTIFACTS {label} {}", directory.display());
    write_executable_script(
        &directory.join("queue-child.py"),
        &PROVIDER
            .replace("PREFIX", &format!("{:?}", prefix.display().to_string()))
            .replace("CASE_LABEL", &format!("{label:?}")),
    );
    write_executable_script(
        &directory.join("queue.py"),
        &format!(
            r#"#!/usr/bin/env python3
import hashlib, json, os, subprocess, sys
prefix = {prefix:?}
child_path = {child_path:?}
with open(prefix + ".fixture-hashes", "w") as ledger:
    ledger.write(json.dumps({{path: hashlib.sha256(open(path, "rb").read()).hexdigest() for path in [child_path, sys.executable]}}))
stderr = open(prefix + ".child-stderr", "wb")
child = subprocess.Popen([sys.executable, child_path], stderr=stderr)
with open(prefix + ".pids", "w") as ledger:
    ledger.write(json.dumps({{"fixture": child.pid, "reaper": os.getpid()}}))
status = child.wait()
with open(prefix + ".exits", "w") as ledger:
    ledger.write(json.dumps({{"fixture": child.pid, "reaper": os.getpid(), "exit": status}}))
sys.exit(status)
"#,
            prefix = prefix.display().to_string(),
            child_path = directory.join("queue-child.py").display().to_string()
        ),
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "queue-provider"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "queue.py"
[contributes]
providers = true
"#,
    )
    .unwrap();
    let registry = Arc::new(ExtensionProviderRegistry::new());
    let mut runtime = ExtensionRuntimeConfig::new(&directory);
    runtime.provider_registry = Some(Arc::clone(&registry));
    if mutation == "timeout" {
        runtime.provider_stream_idle_timeout = Duration::from_millis(100);
    }
    let process = ExtensionProcess::start(trusted_descriptor(&directory, manifest), runtime)
        .await
        .unwrap_or_else(|error| panic!("{label} startup: {error:?}"));
    let mut events = process.subscribe();
    let diagnostics_path = prefix.with_extension("diagnostics");
    let diagnostics = tokio::spawn(async move {
        let mut log = std::fs::File::create(diagnostics_path).unwrap();
        loop {
            match events.recv().await {
                Ok(event) => writeln!(log, "{event:?}").unwrap(),
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    writeln!(log, "LAGGED {count}").unwrap();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let route = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(route) = registry.resolve("queue-provider", "selected") {
                break route;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|error| panic!("{label} catalog timeout: {error:?}"));
    let connection = read_std_lock(&process.inner.connection).clone();
    let slots = read_std_lock(&connection.slots).clone();
    let available = slots.available_permits();
    let mut slot_hold = if capacity == "slots" {
        Some(
            slots
                .clone()
                .acquire_many_owned(available as u32)
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let mut writer_holds = Vec::new();
    if capacity == "writer" {
        for _ in 0..connection.writer.max_capacity() {
            writer_holds.push(connection.writer.clone().reserve_owned().await.unwrap());
        }
    }
    let preentry = matches!(capacity, "prepared" | "constructor");
    let selected = HostStreamModel {
        id: octet_ai::ModelId("queue-provider/selected".into()),
        protocol: Protocol::OpenAiChat,
        pricing: None,
    };
    let transport = if capacity == "constructor" {
        process
            .prepare_provider_stream_transport_inner(&route, selected.clone(), || {
                if !matches!(mutation, "none" | "wrong-model") {
                    mutate_prepared_registry(&registry, &route, mutation);
                }
            })
            .unwrap()
    } else if capacity == "prepared" {
        let transport = process
            .prepare_provider_stream_transport(&route, selected.clone())
            .unwrap();
        if !matches!(mutation, "none" | "wrong-model") {
            mutate_prepared_registry(&registry, &route, mutation);
        }
        transport
    } else {
        process.provider_stream_transport(route.clone())
    };
    assert!(slots.available_permits() > 0 || !preentry);
    if preentry {
        assert_eq!(
            connection.writer.capacity(),
            connection.writer.max_capacity()
        );
    }
    let mut stream_model = selected;
    if mutation == "wrong-model" {
        stream_model.id = octet_ai::ModelId("queue-provider/unselected".into());
    }
    let opening = tokio::spawn(async move {
        transport
            .stream(
                stream_model,
                octet_ai::Request {
                    system: None,
                    messages: Vec::new(),
                    tools: Vec::new(),
                    tool_choice: Default::default(),
                    max_output_tokens: None,
                    temperature: None,
                    stop: Vec::new(),
                    reasoning: Default::default(),
                    reasoning_mode: Default::default(),
                    responses: None,
                    output_format: Default::default(),
                    output_modalities: Default::default(),
                    compatibility: Default::default(),
                    cache_retention: Default::default(),
                    session_id: None,
                },
                Vec::new(),
            )
            .await
    });
    if !preentry {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if lock_std_mutex(&connection.provider_streams).len() == 1
                    && (capacity == "slots" || slots.available_permits() < available)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|error| panic!("{label} capacity wait timeout: {error:?}"));
        if capacity == "slots" && matches!(mutation, "selected" | "withdraw" | "unrelated") {
            std::fs::write(prefix.with_extension("mutate"), mutation).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !prefix.with_extension("mutated").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap_or_else(|error| panic!("{label} mutation acknowledgment timeout: {error:?}"));
            assert_eq!(
                std::fs::read_to_string(prefix.with_extension("mutated")).unwrap(),
                mutation,
                "{label} mutation acknowledgment"
            );
        } else if !matches!(mutation, "none" | "cancel" | "timeout") {
            mutate_prepared_registry(&registry, &route, mutation);
        }
    }
    if mutation == "cancel" {
        opening.abort();
    }
    if !matches!(mutation, "cancel" | "timeout") {
        drop(slot_hold.take());
        writer_holds.clear();
    }
    let joined = tokio::time::timeout(Duration::from_secs(5), opening)
        .await
        .unwrap_or_else(|error| panic!("{label} opening timeout: {error:?}"));
    let result = match joined {
        Ok(result) => Some(result),
        Err(error) => {
            assert!(
                mutation == "cancel" && error.is_cancelled(),
                "{label} opening task: {error:?}"
            );
            None
        }
    };
    drop(slot_hold.take());
    writer_holds.clear();
    let accepted = matches!(&result, Some(Ok(_)));
    let mut completed = false;
    if let Some(Ok(mut stream)) = result {
        std::fs::write(prefix.with_extension("release"), "release").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut text = String::new();
            while let Some(event) = futures_util::StreamExt::next(&mut stream).await {
                match event {
                    Ok(StreamEvent::TextDelta { delta, .. }) => text.push_str(&delta),
                    Ok(StreamEvent::Finished(response)) => {
                        completed =
                            text == "queue-ok" && response.stop_reason == StopReason::EndTurn;
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or_else(|error| panic!("{label} stream timeout: {error:?}"));
    }
    let callbacks = std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default();
    let before = process.health_snapshot();
    eprintln!("CASE PRE SHUTDOWN {label} accepted={accepted} callbacks={callbacks:?} completed={completed} health={before:?} closed={} draining={} pending={} ingress={} permits={} registry={:?}", connection.closed.load(Ordering::Acquire), connection.draining.load(Ordering::Acquire), lock_std_mutex(&connection.pending).len(), lock_std_mutex(&connection.provider_streams).len(), slots.available_permits(), registry.snapshot());
    let started = Instant::now();
    let graceful = process.shutdown().await;
    eprintln!(
        "CASE SHUTDOWN {label} graceful={graceful} elapsed={:?} health={:?}",
        started.elapsed(),
        process.health_snapshot()
    );
    diagnostics.abort();
    let _ = diagnostics.await;
    assert!(
        graceful,
        "{label} shutdown; artifacts={}",
        directory.display()
    );
    let trace = std::fs::read_to_string(prefix.with_extension("trace")).unwrap();
    let environment = trace
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find_map(|value| value["event"].get("environment").cloned())
        .expect("fixture environment receipt");
    for name in ["HOME", "TMPDIR", "PATH", "LANG"] {
        assert_eq!(
            environment[name].as_str(),
            std::env::var(name).ok().as_deref(),
            "{label} fixture environment {name}"
        );
    }
    let pids: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(prefix.with_extension("pids")).unwrap())
            .unwrap();
    let exit: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(prefix.with_extension("exits")).unwrap())
            .unwrap();
    assert_eq!(exit["exit"], 0);
    assert_eq!(exit["fixture"], pids["fixture"]);
    assert_eq!(exit["reaper"], pids["reaper"]);
    let shutdown_acks = trace
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|value| value["event"]["send"]["result"]["terminal"] == "shutdown")
        .count();
    assert_eq!(shutdown_acks, 1);
    let callbacks = std::fs::read_to_string(prefix.with_extension("streams")).unwrap_or_default();
    assert!(
        callbacks.is_empty()
            || callbacks.lines().collect::<Vec<_>>()
                == vec![pids["fixture"].as_u64().unwrap().to_string()]
    );
    eprintln!("CASE FIXTURE WAIT {label} {exit}");
    eprintln!("CASE FINAL CALLBACKS {label} {callbacks:?}");
    (accepted, !callbacks.is_empty(), completed)
}

fn mutate_prepared_registry(
    registry: &ExtensionProviderRegistry,
    route: &crate::extension_provider::ExtensionProviderRoute,
    mutation: &str,
) {
    let (_, mut entries) = registry.snapshot();
    let mut entry = entries.pop().unwrap();
    match mutation {
        "identical" => {}
        "selected" => {
            entry
                .models
                .iter_mut()
                .find(|model| model.id == "selected")
                .unwrap()
                .context_window += 1;
        }
        "unrelated" => {
            entry
                .models
                .iter_mut()
                .find(|model| model.id == "unselected")
                .unwrap()
                .display_name = Some("unselected changed".into());
        }
        "withdraw" => {
            registry.unregister(&route.owner, "queue-provider").unwrap();
        }
        "revoke" => {
            registry
                .set_authorization_status(
                    &route.owner,
                    "queue-provider",
                    crate::extension_provider::ExtensionProviderAuthorizationStatus::Revoked,
                )
                .unwrap();
        }
        "selected-restored" => {
            let mut changed = entry.models.clone();
            changed
                .iter_mut()
                .find(|model| model.id == "selected")
                .unwrap()
                .context_window += 1;
            registry
                .update(
                    route.owner.clone(),
                    api_v03::ProviderUpdateParams {
                        provider: entry.provider.clone(),
                        models: changed,
                    },
                )
                .unwrap();
            registry
                .update(
                    route.owner.clone(),
                    api_v03::ProviderUpdateParams {
                        provider: entry.provider.clone(),
                        models: entry.models.clone(),
                    },
                )
                .unwrap();
        }
        "revoke-restored" => {
            for status in [
                crate::extension_provider::ExtensionProviderAuthorizationStatus::Revoked,
                crate::extension_provider::ExtensionProviderAuthorizationStatus::Ready,
            ] {
                registry
                    .set_authorization_status(&route.owner, "queue-provider", status)
                    .unwrap();
            }
        }
        "withdraw-restored" => {
            registry.unregister(&route.owner, "queue-provider").unwrap();
            registry
                .register(
                    route.owner.clone(),
                    api_v03::ProviderRegisterParams {
                        provider: entry.provider.clone(),
                        models: entry.models.clone(),
                    },
                )
                .unwrap();
        }
        "generation" => {
            let owner = ExtensionProviderOwner {
                generation: route.owner.generation + 1,
                ..route.owner.clone()
            };
            registry
                .register(
                    owner.clone(),
                    api_v03::ProviderRegisterParams {
                        provider: entry.provider.clone(),
                        models: entry.models.clone(),
                    },
                )
                .unwrap();
            registry.complete_initial_catalog(&owner);
        }
        _ => unreachable!(),
    }
    if matches!(mutation, "selected" | "unrelated" | "identical") {
        registry
            .update(
                route.owner.clone(),
                api_v03::ProviderUpdateParams {
                    provider: entry.provider,
                    models: entry.models,
                },
            )
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_provider_admission_preserves_constructor_authority() {
    let mut violations = Vec::new();
    for phase in ["prepared", "constructor"] {
        for mutation in [
            "none",
            "identical",
            "unrelated",
            "selected-restored",
            "revoke-restored",
            "generation",
            "wrong-model",
        ] {
            let outcome = queued_case(phase, mutation).await;
            let valid = if matches!(mutation, "none" | "identical" | "unrelated") {
                outcome == (true, true, true)
            } else {
                outcome == (false, false, false)
            };
            if !valid {
                violations.push((phase, mutation, outcome));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "prepared admission violations: {violations:?}"
    );
}

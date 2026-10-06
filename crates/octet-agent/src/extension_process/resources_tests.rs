//! Real production ExtensionProcess tests against independent Python and Rust
//! low-level peers. These are host-wire tests, NOT SDK author-syntax parity.
use super::tests::{trusted_descriptor, write_executable_script};
use super::*;
use serde_json::{json, Value};
use tempfile::TempDir;

const PYTHON: &str = include_str!("resource_fixture.py");
mod bulk;
mod composition;
const RUST: &str = include_str!("resource_fixture.rs");
static FIXTURE_SERIAL: AtomicU64 = AtomicU64::new(0);
static RUST_EXECUTABLE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("fixture.rs");
    std::fs::write(&source, RUST).unwrap();
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let library = std::fs::read_dir(&deps)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("libserde_json-")
                && p.extension().is_some_and(|e| e == "rlib")
        })
        .max_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap())
        .expect("built serde_json rlib required");
    let executable = temp.path().join("fixture");
    let result = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg("-Cdebuginfo=0")
        .arg(&source)
        .arg("--extern")
        .arg(format!("serde_json={}", library.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("rustc required, never skip");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::read(executable).unwrap()
});

fn ref_schema() -> Value {
    json!({"type":"object","properties":{"$resource":{"type":"string"},"type":{"type":"string","const":"demo.Circuit"}},"required":["$resource","type"],"additionalProperties":false})
}
fn catalog() -> Value {
    let controls = json!({"block":{"type":"boolean"},"invalid":{"type":"boolean"},"error":{"type":"boolean"},"rpc_error":{"type":"boolean"},"registrations":{"type":"integer","minimum":1,"maximum":33}});
    let mut uses = controls.clone();
    uses["resource"] = ref_schema();
    uses["second"] = ref_schema();
    json!([
        {"name":"create","description":"Create native circuit","parameters":{"type":"object","properties":controls,"additionalProperties":false},
         "output_schema":{"type":"object","properties":{"resource":ref_schema(),"second":ref_schema()},"required":["resource"],"additionalProperties":false},
         "operation":{"id":"demo.create","resource_inputs":[],"resource_outputs":[{"path":"/resource","type":"demo.Circuit"},{"path":"/second","type":"demo.Circuit"}]}},
        {"name":"use","description":"Mutate native circuit","parameters":{"type":"object","properties":uses,"required":["resource"],"additionalProperties":false},
         "output_schema":{"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false},
         "operation":{"id":"demo.use","receiver":"/resource","resource_inputs":[{"path":"/resource","type":"demo.Circuit","access":"exclusive"},{"path":"/second","type":"demo.Circuit","access":"exclusive"}],"resource_outputs":[]}},
        {"name":"idle","description":"Barrier","parameters":{"type":"object","properties":controls,"additionalProperties":false},"output_schema":{"type":"object","additionalProperties":false}},
        {"name":"release","description":"Lifecycle reverse release","parameters":{"type":"object","properties":{"resource":ref_schema()},"required":["resource"],"additionalProperties":false}}
    ])
}

struct Fixture {
    temp: TempDir,
    process: ExtensionProcess,
    events: broadcast::Receiver<ExtensionEvent>,
    rust: bool,
    controlled: bool,
    serial: u64,
}
impl Fixture {
    async fn start(rust: bool, controlled: bool, slots: usize) -> Self {
        Self::start_with_supervision(rust, controlled, slots, false).await
    }
    async fn start_with_supervision(
        rust: bool,
        controlled: bool,
        slots: usize,
        supervise: bool,
    ) -> Self {
        Self::start_custom(rust, controlled, slots, supervise, None, catalog(), true).await
    }
    async fn start_custom(
        rust: bool,
        controlled: bool,
        slots: usize,
        supervise: bool,
        bulk: Option<crate::BulkStorage>,
        tools: Value,
        resources: bool,
    ) -> Self {
        let temp = TempDir::new().unwrap();
        let executable = temp.path().join("extension");
        if rust {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&executable, &*RUST_EXECUTABLE).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        } else {
            write_executable_script(&executable, PYTHON);
        }
        std::fs::write(temp.path().join("catalog.json"), tools.to_string()).unwrap();
        let mut features = if resources {
            vec![
                EXTENSION_FEATURE_RESOURCE_REFS_V1,
                EXTENSION_FEATURE_OPERATION_DESCRIPTORS_V1,
            ]
        } else {
            Vec::new()
        };
        if bulk.is_some() {
            features.push(EXTENSION_FEATURE_BULK_OBJECTS_V1);
        }
        std::fs::write(
            temp.path().join("features.json"),
            serde_json::to_vec(&features).unwrap(),
        )
        .unwrap();
        let mut manifest = ExtensionManifest::parse("name='resource-fixture'\nversion='1.0.0'\napi_version='0.4'\n[entrypoint]\ncommand='extension'\n[contributes]\ntools=['create','use','idle','release']\nnotifications=true\n").unwrap();
        manifest
            .entrypoint
            .env
            .insert("HOME".into(), temp.path().to_string_lossy().into());
        manifest.contributes.tools = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        let mut config = ExtensionRuntimeConfig::new(temp.path());
        config.bulk_store = bulk;
        config.supervise = supervise;
        config.max_pending_requests = slots;
        config.request_timeout = Duration::from_secs(10);
        config.cancellation_grace = Duration::from_secs(5);
        config.shutdown_timeout = Duration::from_millis(500);
        let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
            .await
            .unwrap();
        let events = process.subscribe();
        Self {
            temp,
            process,
            events,
            rust,
            controlled,
            serial: FIXTURE_SERIAL.fetch_add(1, Ordering::Relaxed),
        }
    }
    async fn call(
        &self,
        owner: &str,
        name: &str,
        args: Value,
    ) -> Result<ToolCallOutput, ExtensionRuntimeError> {
        call(
            self.process.clone(),
            self.controlled,
            owner.to_owned(),
            name.to_owned(),
            args,
            CancellationToken::default(),
        )
        .await
    }
    async fn create(&self) -> ResourceRef {
        reference(&self.call("A", "create", json!({})).await.unwrap())
    }
    async fn event(&mut self, kind: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let ExtensionEvent::Notification { notification } =
                    self.events.recv().await.unwrap()
                {
                    if let Ok(value) = serde_json::from_str::<Value>(&notification.message) {
                        if value["kind"] == kind {
                            return value;
                        }
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "missing {kind}; rust={}, controlled={}, log={:?}",
                self.rust,
                self.controlled,
                self.log()
            )
        })
    }
    fn allow(&self, request: u64) {
        let connection = read_std_lock(&self.process.inner.connection);
        assert!(connection.queue_notification("fixture/allow", json!({"request":request})));
    }
    fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.temp.path().join("calls.jsonl"))
            .unwrap()
            .split_inclusive('\n')
            // Disposal may append concurrently. Only LF-terminated records
            // are committed log entries, exactly like the JSONL transport.
            .filter(|line| line.ends_with('\n'))
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    fn calls(&self) -> usize {
        self.log().iter().filter(|v| v["kind"] == "call").count()
    }
    fn blocked(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<Result<ToolCallOutput, ExtensionRuntimeError>> {
        tokio::spawn(call(
            self.process.clone(),
            self.controlled,
            "A".into(),
            name.into(),
            args,
            cancel,
        ))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        read_std_lock(&self.process.inner.connection).kill_process_group();
        if let Some(directory) = std::env::var_os("OCTET_RESOURCE_EVIDENCE_DIR") {
            let directory = PathBuf::from(directory).join(format!(
                "{}-{}-{}-{}",
                std::process::id(),
                self.serial,
                if self.rust { "rust" } else { "python" },
                if self.controlled {
                    "controlled"
                } else {
                    "direct"
                }
            ));
            std::fs::create_dir_all(&directory).unwrap();
            for name in ["calls.jsonl", "catalog.json"] {
                std::fs::copy(self.temp.path().join(name), directory.join(name)).unwrap();
            }
            std::fs::write(
                directory.join("fixture-source.sha256"),
                format!(
                    "{:x}",
                    Sha256::digest(if self.rust {
                        RUST.as_bytes()
                    } else {
                        PYTHON.as_bytes()
                    })
                ),
            )
            .unwrap();
        }
    }
}
async fn call(
    process: ExtensionProcess,
    controlled: bool,
    owner: String,
    name: String,
    args: Value,
    cancel: CancellationToken,
) -> Result<ToolCallOutput, ExtensionRuntimeError> {
    let context = process.current_context_for_resource_owner(owner);
    if controlled {
        let connection = read_std_lock(&process.inner.connection).clone();
        let definition = read_std_lock(&connection.tool_catalog)
            .iter()
            .find(|d| d.name == name)
            .unwrap()
            .clone();
        let revision = connection.catalog_revision.load(Ordering::Acquire);
        let (started, _rx) = oneshot::channel();
        process
            .call_tool_controlled(
                connection,
                definition,
                revision,
                args,
                context,
                cancel,
                ToolProgressSink::null(),
                started,
            )
            .await
    } else {
        process.call_tool(name, args, context).await
    }
}
fn reference(output: &ToolCallOutput) -> ResourceRef {
    serde_json::from_value(output.structured_content.as_ref().unwrap()["resource"].clone()).unwrap()
}
fn unavailable(result: Result<ToolCallOutput, ExtensionRuntimeError>) {
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("resource_unavailable"));
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
const VARIANTS: [(bool, bool); 4] = [(false, false), (false, true), (true, false), (true, true)];

#[tokio::test]
async fn r01_lifetime() {
    for (rust, controlled) in VARIANTS {
        let fixture = Fixture::start(rust, controlled, 4).await;
        let resource = fixture.create().await;
        for count in 1..=2 {
            assert_eq!(
                fixture
                    .call("A", "use", json!({"resource":resource}))
                    .await
                    .unwrap()
                    .structured_content
                    .unwrap()["count"],
                count
            );
        }
        assert!(
            fixture
                .process
                .release_resource("A", &resource)
                .unwrap()
                .retired
        );
        let calls = fixture.calls();
        unavailable(fixture.call("A", "use", json!({"resource":resource})).await);
        assert_eq!(fixture.calls(), calls);
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r02_wrong_type_r03_fabricated_r04_foreign_session_r05_foreign_extension() {
    for (rust, controlled) in VARIANTS {
        let fixture = Fixture::start(rust, controlled, 4).await;
        let foreign = Fixture::start(rust, controlled, 4).await;
        let resource = fixture.create().await;
        let calls = fixture.calls();
        let wrong = ResourceRef {
            resource_type: "demo.Other".into(),
            ..resource.clone()
        };
        assert!(fixture
            .call("A", "use", json!({"resource":wrong}))
            .await
            .unwrap_err()
            .to_string()
            .contains("resource_type_mismatch"));
        unavailable(fixture.call("B", "use", json!({"resource":resource})).await);
        unavailable(
            fixture
                .call(
                    "A",
                    "use",
                    json!({"resource":{"$resource":"fabricated","type":"demo.Circuit"}}),
                )
                .await,
        );
        unavailable(foreign.call("A", "use", json!({"resource":resource})).await);
        for (name, arguments) in [("use", json!({})), ("create", json!({}))] {
            unavailable(
                fixture
                    .process
                    .call_tool(name, arguments, fixture.process.current_context())
                    .await,
            );
        }
        assert_eq!(fixture.calls(), calls);
        assert_eq!(foreign.calls(), 0);
        fixture.process.shutdown().await;
        foreign.process.shutdown().await;
    }
}

#[tokio::test]
async fn r06_stale_generation_r12_reload_r13_failed_reload() {
    for (rust, controlled) in VARIANTS {
        let fixture = Fixture::start(rust, controlled, 4).await;
        let resource = fixture.create().await;
        std::fs::write(fixture.temp.path().join("catalog.json"), "[]").unwrap();
        assert!(fixture.process.reload().await.is_err());
        fixture
            .call("A", "use", json!({"resource":resource}))
            .await
            .unwrap();
        std::fs::write(
            fixture.temp.path().join("catalog.json"),
            catalog().to_string(),
        )
        .unwrap();
        fixture.process.reload().await.unwrap();
        let calls = fixture.calls();
        unavailable(fixture.call("A", "use", json!({"resource":resource})).await);
        assert_eq!(fixture.calls(), calls);
        assert_ne!(fixture.create().await, resource);
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r07_queued_release() {
    for (rust, controlled) in VARIANTS {
        let mut fixture = Fixture::start(rust, controlled, 1).await;
        let resource = fixture.create().await;
        let busy = fixture.blocked("idle", json!({"block":true}), CancellationToken::default());
        let entered = loop {
            let e = fixture.event("entered").await;
            if e["name"] == "idle" {
                break e;
            }
        };
        let queued = fixture.blocked(
            "use",
            json!({"resource":resource}),
            CancellationToken::default(),
        );
        until(|| {
            read_std_lock(&fixture.process.inner.connection)
                .active_admissions
                .load(Ordering::Acquire)
                == 2
        })
        .await;
        assert!(fixture.process.release_resource("A", &resource).is_ok());
        let calls = fixture.calls();
        fixture.allow(entered["request"].as_u64().unwrap());
        busy.await.unwrap().unwrap();
        unavailable(queued.await.unwrap());
        assert_eq!(fixture.calls(), calls);
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn malformed_terminal_tears_down_generation_instead_of_unlocking_native_execution() {
    for (rust, controlled) in VARIANTS {
        let mut fixture = Fixture::start(rust, controlled, 4).await;
        let resource = fixture.create().await;
        let running = fixture.blocked(
            "use",
            json!({"resource":resource,"block":true}),
            CancellationToken::default(),
        );
        let entered = loop {
            let event = fixture.event("entered").await;
            if event["name"] == "use" {
                break event;
            }
        };
        let connection = read_std_lock(&fixture.process.inner.connection).clone();
        assert!(connection
            .queue_notification("fixture/malformed", json!({"request":entered["request"]})));
        assert!(running.await.unwrap().is_err());
        assert!(connection.closed.load(Ordering::Acquire));
        let calls = fixture.calls();
        assert!(fixture.process.lookup_resource("A", &resource).is_err());
        assert!(fixture
            .call("A", "use", json!({"resource":resource}))
            .await
            .is_err());
        assert_eq!(fixture.calls(), calls);
        assert!(!fixture.log().iter().any(|v| v["kind"] == "dispose"));
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r08_admission_release_and_all_resource_pins() {
    for (rust, controlled) in VARIANTS {
        let mut fixture = Fixture::start(rust, controlled, 4).await;
        let a = fixture.create().await;
        let b = fixture.create().await;
        let running = fixture.blocked(
            "use",
            json!({"resource":a,"second":b,"block":true}),
            CancellationToken::default(),
        );
        let entered = loop {
            let e = fixture.event("entered").await;
            if e["name"] == "use" {
                break e;
            }
        };
        for r in [&a, &b] {
            assert!(fixture
                .process
                .release_resource("A", r)
                .unwrap_err()
                .to_string()
                .contains("resource_busy"));
        }
        assert_eq!(
            fixture
                .log()
                .iter()
                .filter(|e| e["kind"] == "dispose")
                .count(),
            0
        );
        fixture.allow(entered["request"].as_u64().unwrap());
        running.await.unwrap().unwrap();
        for r in [&a, &b] {
            assert!(fixture.process.release_resource("A", r).is_ok());
        }
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r09_cancel_first_keeps_pin_until_late_terminal() {
    for (rust, controlled) in VARIANTS {
        let mut fixture = Fixture::start(rust, controlled, 4).await;
        let resource = fixture.create().await;
        let cancel = CancellationToken::default();
        let running = fixture.blocked(
            "use",
            json!({"resource":resource,"block":true}),
            cancel.clone(),
        );
        let entered = loop {
            let e = fixture.event("entered").await;
            if e["name"] == "use" {
                break e;
            }
        };
        if controlled {
            cancel.cancel();
            assert!(running.await.unwrap().is_err());
        } else {
            running.abort();
            assert!(running.await.unwrap_err().is_cancelled());
        }
        fixture.event("cancelled").await;
        assert!(fixture
            .process
            .release_resource("A", &resource)
            .unwrap_err()
            .to_string()
            .contains("resource_busy"));
        assert_eq!(
            fixture
                .log()
                .iter()
                .filter(|e| e["kind"] == "dispose")
                .count(),
            0
        );
        fixture.allow(entered["request"].as_u64().unwrap());
        fixture.event("terminal").await;
        until(|| fixture.process.release_resource("A", &resource).is_ok()).await;
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r09_cancel_creation_r20_provisional_isolation() {
    for (rust, controlled) in VARIANTS {
        let mut fixture = Fixture::start(rust, controlled, 4).await;
        let cancel = CancellationToken::default();
        let running = fixture.blocked("create", json!({"block":true}), cancel.clone());
        let ready = fixture.event("output_ready").await;
        let resource: ResourceRef = serde_json::from_value(ready["resources"][0].clone()).unwrap();
        assert!(fixture.process.lookup_resource("A", &resource).is_err());
        let count = fixture.calls();
        unavailable(fixture.call("A", "use", json!({"resource":resource})).await);
        assert_eq!(fixture.calls(), count);
        if controlled {
            cancel.cancel();
            assert!(running.await.unwrap().is_err());
        } else {
            running.abort();
            assert!(running.await.unwrap_err().is_cancelled());
        }
        fixture.event("cancelled").await;
        assert_eq!(
            fixture
                .log()
                .iter()
                .filter(|e| e["kind"] == "dispose")
                .count(),
            0
        );
        fixture.allow(ready["request"].as_u64().unwrap());
        fixture.event("disposed").await;
        assert!(fixture.process.lookup_resource("A", &resource).is_err());
        unavailable(fixture.call("A", "use", json!({"resource":resource})).await);
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r11_crash_supervised_restart() {
    for (rust, controlled) in VARIANTS {
        let mut fixture = Fixture::start_with_supervision(rust, controlled, 4, true).await;
        let resource = fixture.create().await;
        let running = fixture.blocked(
            "use",
            json!({"resource":resource,"block":true}),
            CancellationToken::default(),
        );
        loop {
            let e = fixture.event("entered").await;
            if e["name"] == "use" {
                break;
            }
        }
        let old_generation = read_std_lock(&fixture.process.inner.connection).generation;
        read_std_lock(&fixture.process.inner.connection).kill_process_group();
        assert!(running.await.unwrap().is_err());
        until(|| {
            let c = read_std_lock(&fixture.process.inner.connection);
            c.generation > old_generation && connection_is_usable(&c)
        })
        .await;
        let calls = fixture.calls();
        unavailable(fixture.call("A", "use", json!({"resource":resource})).await);
        assert_eq!(fixture.calls(), calls);
        assert_ne!(fixture.create().await, resource);
        assert_eq!(
            fixture
                .log()
                .iter()
                .filter(|e| e["kind"] == "call" && e["name"] == "use")
                .count(),
            1
        );
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r10_complete_first() {
    for (rust, controlled) in VARIANTS {
        let fixture = Fixture::start(rust, controlled, 4).await;
        let cancel = CancellationToken::default();
        let result = call(
            fixture.process.clone(),
            controlled,
            "A".into(),
            "create".into(),
            json!({}),
            cancel.clone(),
        )
        .await
        .unwrap();
        cancel.cancel();
        let resource = reference(&result);
        fixture.process.lookup_resource("A", &resource).unwrap();
        fixture
            .call("A", "use", json!({"resource":resource}))
            .await
            .unwrap();
        fixture.process.shutdown().await;
    }
}

#[test]
fn r14_host_restart() {
    const CHILD: &str = "OCTET_RESOURCE_RESTART_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        let root = PathBuf::from(std::env::var_os("OCTET_RESOURCE_RESTART_ROOT").unwrap());
        let rust = mode.starts_with("rust");
        let controlled = mode.contains("controlled");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let fixture = Fixture::start(rust, controlled, 4).await;
                assert_eq!(
                    read_std_lock(&fixture.process.inner.connection).generation,
                    1
                );
                if mode.ends_with("create") {
                    let resource = fixture.create().await;
                    std::fs::write(
                        root.join("saved.json"),
                        json!({"resource":resource,"host_pid":std::process::id()}).to_string(),
                    )
                    .unwrap();
                } else {
                    let saved: Value =
                        serde_json::from_slice(&std::fs::read(root.join("saved.json")).unwrap())
                            .unwrap();
                    assert_ne!(saved["host_pid"], std::process::id());
                    let resource: ResourceRef =
                        serde_json::from_value(saved["resource"].clone()).unwrap();
                    unavailable(fixture.call("A", "use", json!({"resource":resource})).await);
                    assert_eq!(fixture.calls(), 0);
                    assert_ne!(fixture.create().await, resource);
                }
                fixture.process.shutdown().await;
            });
        return;
    }
    for (rust, controlled) in VARIANTS {
        let root = TempDir::new().unwrap();
        for phase in ["create", "reuse"] {
            let mode = format!(
                "{}-{}-{phase}",
                if rust { "rust" } else { "python" },
                if controlled { "controlled" } else { "direct" }
            );
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "extension_process::resources_tests::r14_host_restart",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(CHILD, &mode)
                .env("OCTET_RESOURCE_RESTART_ROOT", root.path())
                .output()
                .expect("actual independent host process required");
            if let Some(directory) = std::env::var_os("OCTET_RESOURCE_EVIDENCE_DIR") {
                let base =
                    PathBuf::from(directory).join(format!("{}-host-{mode}", std::process::id()));
                std::fs::write(base.with_extension("stdout"), &output.stdout).unwrap();
                std::fs::write(base.with_extension("stderr"), &output.stderr).unwrap();
                std::fs::write(
                    base.with_extension("exit"),
                    output.status.code().unwrap_or(-1).to_string(),
                )
                .unwrap();
            }
            assert!(
                output.status.success(),
                "host restart child failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }
}

#[tokio::test]
async fn r15_invalid_parent_output_r16_failed_parent_r21_output_atomicity() {
    for (rust, controlled) in VARIANTS {
        for failure in ["invalid", "error", "rpc_error"] {
            let mut fixture = Fixture::start(rust, controlled, 4).await;
            let args = json!({"registrations":2, failure:true});
            let result = fixture.call("A", "create", args).await;
            if failure == "error" {
                assert!(result.unwrap().is_error);
            } else {
                assert!(result.is_err());
            }
            let ready = fixture.event("output_ready").await;
            fixture.event("disposed").await;
            for value in ready["resources"].as_array().unwrap() {
                let reference: ResourceRef = serde_json::from_value(value.clone()).unwrap();
                assert!(fixture.process.lookup_resource("A", &reference).is_err());
            }
            fixture.process.shutdown().await;
        }
    }
}

#[tokio::test]
async fn r17_cleanup_failure_r18_cleanup_hang() {
    for (rust, controlled) in VARIANTS {
        for mode in ["failed", "hang"] {
            let fixture = Fixture::start(rust, controlled, 4).await;
            let resource = fixture.create().await;
            std::fs::write(fixture.temp.path().join("cleanup-mode"), mode).unwrap();
            let released = fixture.process.release_resource("A", &resource).unwrap();
            assert_eq!(released.cleanup, ResourceCleanupStatus::Pending);
            assert!(fixture.process.lookup_resource("A", &resource).is_err());
            until(|| {
                let connection = read_std_lock(&fixture.process.inner.connection);
                let registry = lock_std_mutex(&connection.resources);
                registry.cleanup_status(&resource)
                    == Some(if mode == "failed" {
                        ResourceCleanupStatus::Failed
                    } else {
                        ResourceCleanupStatus::Unknown
                    })
            })
            .await;
            assert!(fixture.process.lookup_resource("A", &resource).is_err());
            fixture.process.shutdown().await;
        }
    }
}

#[tokio::test]
async fn r19_resource_quota() {
    for (rust, controlled) in VARIANTS {
        let mut fixture = Fixture::start(rust, controlled, 4).await;
        assert!(
            fixture
                .call("A", "create", json!({"registrations":33}))
                .await
                .unwrap()
                .is_error
        );
        fixture.event("disposed").await;
        let mut refs = Vec::new();
        for _ in 0..MAX_RESOURCE_RECORDS {
            refs.push(fixture.create().await);
        }
        assert!(
            fixture
                .call("A", "create", json!({}))
                .await
                .unwrap()
                .is_error
        );
        fixture.process.release_resource("A", &refs[0]).unwrap();
        until(|| {
            fixture
                .process
                .release_resource("A", &refs[0])
                .is_ok_and(|r| r.cleanup == ResourceCleanupStatus::Completed)
        })
        .await;
        fixture.create().await;
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r22_owner_roundtrip() {
    for (rust, controlled) in VARIANTS {
        let fixture = Fixture::start(rust, controlled, 4).await;
        fixture.process.set_host_state(ExtensionHostState {
            session_id: Some("A".into()),
            ..ExtensionHostState::default()
        });
        let old = fixture.create().await;
        fixture.process.set_host_state(ExtensionHostState {
            session_id: Some("B".into()),
            ..ExtensionHostState::default()
        });
        let b = reference(&fixture.call("B", "create", json!({})).await.unwrap());
        fixture.process.set_host_state(ExtensionHostState {
            session_id: Some("A".into()),
            ..ExtensionHostState::default()
        });
        let a = fixture.create().await;
        assert_ne!(old, a);
        let calls = fixture.calls();
        unavailable(fixture.call("A", "use", json!({"resource":old})).await);
        unavailable(fixture.call("B", "use", json!({"resource":b})).await);
        assert_eq!(fixture.calls(), calls);
        fixture.process.shutdown().await;
    }
}

#[test]
fn resource_limits_reject_unenforced_peer_bounds() {
    let manifest = ExtensionManifest::parse("name='limits-fixture'\nversion='1.0.0'\napi_version='0.4'\n[entrypoint]\ncommand='fixture'\n").unwrap();
    for (records, registrations, accepted) in [
        (256, 32, true),
        (255, 32, false),
        (256, 31, false),
        (257, 32, false),
        (0, 0, false),
    ] {
        let response: InitializeResponse = serde_json::from_value(json!({"api_version":"0.4","tools":[],"protocol":{"version":"0.4","features":["request_cancellation","content_parts","resource_refs_v1"],"limits":{"max_concurrent_requests":4,"resource_refs_v1":{"max_records":records,"max_registrations_per_parent":registrations}}}})).unwrap();
        assert_eq!(
            negotiate_contributions_with_host_services(
                &manifest,
                response,
                4,
                OfferedHostServices::default()
            )
            .is_ok(),
            accepted
        );
    }
}

#[test]
fn r24_descriptor_validation() {
    let tool: ToolDefinition = serde_json::from_value(catalog()[1].clone()).unwrap();
    validate_operation_definition(&tool, EXTENSION_API_VERSION_0_4).unwrap();
    let mut enum_tool = tool.clone();
    enum_tool.parameters["properties"]["resource"]["properties"]["type"] =
        json!({"type":"string","enum":["demo.Circuit"]});
    validate_operation_definition(&enum_tool, EXTENSION_API_VERSION_0_4).unwrap();
    for nominal in [
        json!({"type":"string"}),
        json!({"type":"string","enum":["demo.Other"]}),
        json!({"type":"string","const":"demo.Circuit","enum":["demo.Other"]}),
    ] {
        let mut invalid = tool.clone();
        invalid.parameters["properties"]["resource"]["properties"]["type"] = nominal;
        assert!(validate_operation_definition(&invalid, EXTENSION_API_VERSION_0_4).is_err());
    }
    for path in ["resource", "/missing", "/resource/~2", "/resource/0"] {
        let mut invalid = tool.clone();
        invalid.operation.as_mut().unwrap().resource_inputs[0].path = path.into();
        assert!(validate_operation_definition(&invalid, EXTENSION_API_VERSION_0_4).is_err());
    }
    let mut invalid = tool.clone();
    invalid.operation.as_mut().unwrap().receiver = Some("/missing".into());
    assert!(validate_operation_definition(&invalid, EXTENSION_API_VERSION_0_4).is_err());
    let mut invalid = tool.clone();
    let slot = invalid.operation.as_ref().unwrap().resource_inputs[0].clone();
    invalid
        .operation
        .as_mut()
        .unwrap()
        .resource_inputs
        .push(slot);
    assert!(validate_operation_definition(&invalid, EXTENSION_API_VERSION_0_4).is_err());
    let mut invalid = tool;
    invalid.parameters["properties"]["array"] = json!({"type":"array","items":ref_schema()});
    assert!(validate_operation_definition(&invalid, EXTENSION_API_VERSION_0_4).is_err());
}

#[tokio::test]
async fn r24_descriptor_validation_real_candidate_is_atomic() {
    for (rust, controlled) in VARIANTS {
        let fixture = Fixture::start(rust, controlled, 4).await;
        let resource = fixture.create().await;
        for mutation in 0..4 {
            let mut invalid = catalog();
            match mutation {
                0 => invalid[1]["operation"]["resource_inputs"][0]["path"] = json!("/missing"),
                1 => invalid[1]["operation"]["receiver"] = json!("/absent"),
                2 => {
                    invalid[1]["operation"]["resource_inputs"][1] =
                        invalid[1]["operation"]["resource_inputs"][0].clone()
                }
                _ => {
                    invalid[1]["parameters"]["properties"]["array"] =
                        json!({"type":"array","items":ref_schema()})
                }
            }
            let generation = fixture
                .process
                .current_context_for_resource_owner("A")
                .resource_owner
                .unwrap()
                .process_generation;
            std::fs::write(
                fixture.temp.path().join("catalog.json"),
                invalid.to_string(),
            )
            .unwrap();
            assert!(fixture.process.reload().await.is_err());
            assert_eq!(
                fixture
                    .process
                    .lookup_resource("A", &resource)
                    .unwrap()
                    .process_generation,
                generation
            );
            fixture
                .call("A", "use", json!({"resource":resource}))
                .await
                .unwrap();
        }
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn r23_registered_tool_zero_dispatch() {
    for rust in [false, true] {
        let fixture = Fixture::start(rust, true, 4).await;
        let resource = fixture.create().await;
        let mut host = ExtensionHost::new();
        fixture.process.register_dynamic_tool_catalog(&mut host);
        host.finalize_tool_surface();
        let (_, tools) = host.tool_snapshot();
        let tool = tools.iter().find(|t| t.definition().name == "use").unwrap();
        let sandbox = crate::SandboxConfig::new(fixture.temp.path());
        let mut context = ToolContext {
            workspace: fixture.temp.path(),
            sandbox: &sandbox,
            execution_scope: "scope",
            resource_owner: "B",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: CancellationToken::default(),
        };
        let count = fixture.calls();
        assert!(tool
            .execute(json!({"resource":resource}), &context)
            .await
            .unwrap_err()
            .to_string()
            .contains("resource_unavailable"));
        context.resource_owner = "A";
        let wrong = ResourceRef {
            resource_type: "demo.Other".into(),
            ..resource.clone()
        };
        assert!(tool
            .execute(json!({"resource":wrong}), &context)
            .await
            .unwrap_err()
            .to_string()
            .contains("resource_type_mismatch"));
        fixture.process.release_resource("A", &resource).unwrap();
        assert!(tool
            .execute(json!({"resource":resource}), &context)
            .await
            .unwrap_err()
            .to_string()
            .contains("resource_unavailable"));
        assert_eq!(fixture.calls(), count);
        fixture.process.shutdown().await;
    }
}

#[tokio::test]
async fn owner_correlated_reverse_release() {
    for (rust, controlled) in VARIANTS {
        let fixture = Fixture::start(rust, controlled, 4).await;
        let resource = fixture.create().await;
        assert!(
            !fixture
                .call("A", "release", json!({"resource":resource}))
                .await
                .unwrap()
                .is_error
        );
        assert!(fixture.process.lookup_resource("A", &resource).is_err());
        let calls = fixture.calls();
        unavailable(fixture.call("A", "use", json!({"resource":resource})).await);
        assert_eq!(fixture.calls(), calls);
        fixture.process.shutdown().await;
    }
}

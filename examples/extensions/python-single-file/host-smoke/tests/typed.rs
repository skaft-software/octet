//! Actual production ExtensionProcess acceptance of the source Python typed SDK.
use octet_agent::extension_process::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
    ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[path = "support/progress_capture.rs"]
mod progress_capture;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    process: ExtensionProcess,
}
impl Fixture {
    async fn start() -> Self {
        Self::start_fixture(
            "typed_fixture.py",
            &[
                "typed_roundtrip",
                "invalid_output",
                "typed_wait",
                "typed_progress",
                "typed_diagnostics",
                "malformed_diagnostics",
            ],
        )
        .await
    }
    async fn resources() -> Self {
        Self::start_fixture(
            "resource_fixture.py",
            &[
                "create",
                "increment",
                "invalid_output",
                "failed_parent",
                "hold",
            ],
        )
        .await
    }
    async fn bulk() -> Self {
        Self::start_fixture(
            "bulk_fixture.py",
            &[
                "publish",
                "measure",
                "invalid_output",
                "failed_parent",
                "cancel_publish",
                "cancel_read",
            ],
        )
        .await
    }
    async fn start_fixture(script: &str, tools: &[&str]) -> Self {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .to_path_buf();
        let root = std::env::temp_dir().join(format!(
            "octet-python-typed-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = source.join("sdk/python/tests").join(script);
        assert!(
            fixture.is_file(),
            "source-matched Python fixture is required"
        );
        let manifest_text = format!(
            r#"
name = "python-typed-fixture"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{}, {}]
[entrypoint.env]
HOME = {}
PYTHONDONTWRITEBYTECODE = "1"
[capabilities]
filesystem = "none"
process = false
network = false
[contributes]
tools = {}
"#,
            json!(fixture),
            json!(root.join("calls.jsonl")),
            json!(root),
            json!(tools)
        );
        let manifest_path = root.join("extension.toml");
        fs::write(&manifest_path, &manifest_text).unwrap();
        let manifest = ExtensionManifest::parse(&manifest_text).unwrap();
        let descriptor = DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        };
        let mut config = ExtensionRuntimeConfig::new(&root);
        config.request_timeout = Duration::from_secs(3);
        config.cancellation_grace = Duration::from_millis(300);
        config.supervise = false;
        if matches!(script, "bulk_fixture.py" | "progress_fixture.py") {
            config.bulk_store = Some(octet_agent::BulkStorage::new().unwrap());
        }
        if script == "bulk_matrix_fixture.py" {
            config.bulk_store = Some(
                octet_agent::BulkStorage::with_root_and_limits(
                    root.join("bulk-store"),
                    octet_agent::BulkLimits {
                        object_bytes: 64,
                        owner_bytes: 64,
                        write_tickets_per_generation: 1,
                        read_leases_per_generation: 1,
                        blobs_per_owner: 1,
                    },
                )
                .unwrap(),
            );
        }
        let process = ExtensionProcess::start(descriptor, config).await.unwrap();
        assert_eq!(process.api_version(), "0.4");
        Self { root, process }
    }
    async fn call(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<
        octet_agent::extension_process::ToolCallOutput,
        octet_agent::extension_process::ExtensionRuntimeError,
    > {
        self.process
            .call_tool(name, arguments, self.process.current_context())
            .await
    }
    async fn owned_call(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<
        octet_agent::extension_process::ToolCallOutput,
        octet_agent::extension_process::ExtensionRuntimeError,
    > {
        self.process
            .call_tool(
                name,
                arguments,
                self.process
                    .current_context_for_resource_owner("python-test-owner"),
            )
            .await
    }
    async fn create(&self, args: Value) -> octet_agent::extension_process::ResourceRef {
        let output = self.owned_call("create", args).await.unwrap();
        serde_json::from_value(output.structured_content.unwrap()["counter"].clone()).unwrap()
    }
    fn log(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    async fn barrier(&self, event: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !self.log().iter().any(|row| row["event"] == event) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("child-side barrier reached");
    }
    async fn shutdown(self) {
        assert!(self.process.shutdown().await);
        assert!(!self.process.is_running());
        let log = self.log();
        assert_eq!(
            log.iter().filter(|row| row["event"] == "shutdown").count(),
            1
        );
        let pid = &log[0]["pid"];
        assert!(log.iter().all(|row| &row["pid"] == pid));
        println!("child log: {}", json!(log));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn valid() -> Value {
    json!({"name":"λ😀","enabled":true,"samples":[1.0]})
}
fn common() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../sdk/conformance/typed-values-v1.json"
    ))
    .unwrap()
}

#[tokio::test]
async fn a01_typed_roundtrip() {
    let f = Fixture::start().await;
    let tool = f
        .process
        .tool_definitions()
        .into_iter()
        .find(|tool| tool.name == "typed_roundtrip")
        .unwrap();
    assert_eq!(tool.output_schema.as_ref(), Some(&tool.parameters));
    assert_eq!(tool.parameters["additionalProperties"], false);
    let result = f.call("typed_roundtrip", valid()).await.unwrap();
    assert_eq!(
        result.structured_content,
        Some(json!({"name":"λ😀","enabled":true,"samples":[1.0],"note":null}))
    );
    assert_eq!(result.content, "Echoed typed record.");
    assert!(!result.is_error);
    f.shutdown().await;
}

#[tokio::test]
async fn a02_invalid_input_zero_handler_entries() {
    let f = Fixture::start().await;
    let initial = f.log().len();
    for value in common()["cases"][0]["invalid"].as_array().unwrap() {
        assert!(f.call("typed_roundtrip", value.clone()).await.is_err());
    }
    assert!(f
        .call(
            "typed_roundtrip",
            json!({"name":"x","enabled":true,"samples":[9007199254740992_u64]})
        )
        .await
        .is_err());
    assert_eq!(f.log().len(), initial, "invalid calls entered domain code");
    assert!(!f.call("typed_roundtrip", valid()).await.unwrap().is_error);
    f.shutdown().await;
}

#[tokio::test]
async fn a03_invalid_output_not_admitted() {
    let f = Fixture::start().await;
    assert!(f.call("invalid_output", valid()).await.is_err());
    assert!(!f.call("typed_roundtrip", valid()).await.unwrap().is_error);
    f.shutdown().await;
}

#[tokio::test]
async fn a04_optional_values() {
    let f = Fixture::start().await;
    for value in common()["cases"][0]["valid"].as_array().unwrap() {
        let result = f.call("typed_roundtrip", value.clone()).await.unwrap();
        let mut expected = value.clone();
        if expected.get("note").is_none() {
            expected["note"] = Value::Null;
        }
        // JSON integer input is accepted by a floating-point field and encoded as f64.
        expected["samples"] = expected["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| json!(n.as_f64().unwrap()))
            .collect();
        assert_eq!(result.structured_content, Some(expected));
    }
    f.shutdown().await;
}

#[tokio::test]
async fn a05_diagnostics_retained_and_malformed_refused() {
    let f = Fixture::start().await;
    let result = f.call("typed_diagnostics", valid()).await.unwrap();
    assert!(result.is_error);
    assert!(result.structured_content.is_none());
    let diagnostic = &result.metadata["octet_diagnostics_v1"][0];
    assert_eq!(diagnostic["code"], "fixture.invalid");
    assert_eq!(diagnostic["primary"]["span"]["end_byte"], 1);
    assert_eq!(diagnostic["fixes"][0]["edits"][0]["replacement"], "b");
    assert!(result
        .content
        .contains("error[fixture.invalid]: Fixture domain failure."));
    assert!(f.call("malformed_diagnostics", valid()).await.is_err());
    f.shutdown().await;
}

#[tokio::test]
async fn a06_cancel_one_terminal_same_generation() {
    let f = Fixture::start().await;
    let generation = f.process.health_snapshot().generation;
    let child = f.process.clone();
    let call = tokio::spawn(async move {
        child
            .call_tool("typed_wait", valid(), child.current_context())
            .await
    });
    f.barrier("entered").await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    f.barrier("cancelled").await;
    assert!(!f.call("typed_roundtrip", valid()).await.unwrap().is_error);
    assert_eq!(f.process.health_snapshot().generation, generation);
    assert_eq!(
        f.log()
            .iter()
            .filter(|row| row["event"] == "cancelled")
            .count(),
        1
    );
    f.shutdown().await;
}

#[tokio::test]
async fn a07_progress_not_in_result() {
    let f = Fixture::start().await;
    assert!(f.process.negotiated_features().contains("request_progress"));
    let result = f.call("typed_progress", valid()).await.unwrap();
    assert_eq!(result.content, "Progress finished.");
    assert!(!serde_json::to_string(&result).unwrap().contains("Step"));
    f.shutdown().await;
}

#[tokio::test]
async fn r01_resource_roundtrip_release_and_r02_rejected_reuse() {
    let f = Fixture::resources().await;
    let reference = f.create(json!({"n": 40})).await;
    for expected in [41, 42] {
        let result = f
            .owned_call("increment", json!({"counter": reference}))
            .await
            .unwrap();
        assert_eq!(result.structured_content, Some(json!(expected)));
    }
    let release = f
        .process
        .release_resource("python-test-owner", &reference)
        .unwrap();
    assert!(release.retired);
    f.barrier("disposed").await;
    let before = f.log().len();
    assert!(f
        .owned_call("increment", json!({"counter": reference}))
        .await
        .is_err());
    assert_eq!(f.log().len(), before);
    assert_eq!(
        f.log()
            .iter()
            .find(|row| row["event"] == "disposed")
            .unwrap()["invalidated"],
        true
    );
    f.shutdown().await;
}

#[tokio::test]
async fn r04_wrong_owner_and_nominal_type_do_not_enter_python() {
    let f = Fixture::resources().await;
    let reference = f.create(json!({})).await;
    let before = f.log().len();
    assert!(f
        .process
        .call_tool(
            "increment",
            json!({"counter": reference}),
            f.process
                .current_context_for_resource_owner("foreign-owner")
        )
        .await
        .is_err());
    let mut wrong = serde_json::to_value(&reference).unwrap();
    wrong["type"] = json!("example.Wrong.v1");
    assert!(f
        .owned_call("increment", json!({"counter": wrong}))
        .await
        .is_err());
    assert_eq!(f.log().len(), before);
    f.process
        .release_resource("python-test-owner", &reference)
        .unwrap();
    f.barrier("disposed").await;
    f.shutdown().await;
}

#[tokio::test]
async fn r08_busy_release_preserves_native_execution() {
    let f = Fixture::resources().await;
    let reference = f.create(json!({"n": 7})).await;
    let child = f.process.clone();
    let arguments = json!({"counter": reference});
    let call = tokio::spawn(async move {
        child
            .call_tool(
                "hold",
                arguments,
                child.current_context_for_resource_owner("python-test-owner"),
            )
            .await
    });
    f.barrier("entered").await;
    assert!(f
        .process
        .release_resource("python-test-owner", &reference)
        .unwrap_err()
        .to_string()
        .contains("resource_busy"));
    assert!(!f.log().iter().any(|row| row["event"] == "disposed"));
    fs::write(f.root.join("allow_terminal"), "go").unwrap();
    assert_eq!(
        call.await.unwrap().unwrap().structured_content,
        Some(json!(7))
    );
    f.process
        .release_resource("python-test-owner", &reference)
        .unwrap();
    f.barrier("disposed").await;
    f.shutdown().await;
}

#[tokio::test]
async fn r09_cancel_does_not_dispose_before_execution_settles() {
    let f = Fixture::resources().await;
    let generation = f.process.health_snapshot().generation;
    let reference = f.create(json!({"n": 9})).await;
    let child = f.process.clone();
    let arguments = json!({"counter": reference});
    let call = tokio::spawn(async move {
        child
            .call_tool(
                "hold",
                arguments,
                child.current_context_for_resource_owner("python-test-owner"),
            )
            .await
    });
    f.barrier("entered").await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    assert!(f
        .process
        .release_resource("python-test-owner", &reference)
        .unwrap_err()
        .to_string()
        .contains("resource_busy"));
    assert!(!f.log().iter().any(|row| row["event"] == "disposed"));
    fs::write(f.root.join("allow_terminal"), "go").unwrap();
    f.barrier("settled").await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match f.process.release_resource("python-test-owner", &reference) {
                Ok(_) => break,
                Err(error) if error.to_string().contains("resource_busy") => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("unexpected release failure: {error}"),
            }
        }
    })
    .await
    .unwrap();
    f.barrier("disposed").await;
    assert_eq!(f.process.health_snapshot().generation, generation);
    let events: Vec<_> = f
        .log()
        .iter()
        .map(|row| row["event"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        events.iter().position(|event| event == "settled").unwrap()
            < events.iter().position(|event| event == "disposed").unwrap()
    );
    f.shutdown().await;
}

#[tokio::test]
async fn r15_invalid_output_and_r16_failed_parent_retire_provisional_native_state() {
    for name in ["invalid_output", "failed_parent"] {
        let f = Fixture::resources().await;
        let result = f.owned_call(name, json!({})).await;
        if name == "invalid_output" {
            assert!(result.is_err());
        } else {
            let output = result.unwrap();
            assert!(output.is_error);
            assert!(output.structured_content.is_none());
        }
        f.barrier("disposed").await;
        f.shutdown().await;
    }
}

#[tokio::test]
async fn r17_disposer_failure_is_failed_not_reusable() {
    use octet_agent::extension_process::ResourceCleanupStatus;
    let f = Fixture::resources().await;
    let reference = f.create(json!({"fail_cleanup": true})).await;
    assert!(
        f.process
            .release_resource("python-test-owner", &reference)
            .unwrap()
            .retired
    );
    f.barrier("disposed").await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let release = f
                .process
                .release_resource("python-test-owner", &reference)
                .unwrap();
            if release.cleanup == ResourceCleanupStatus::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(f
        .owned_call("increment", json!({"counter": reference}))
        .await
        .is_err());
    f.shutdown().await;
}

#[tokio::test]
async fn b01_large_binary_lifecycle_and_b04_metadata_integrity() {
    let f = Fixture::bulk().await;
    let output = f.owned_call("publish", json!({})).await.unwrap();
    let reference = output.structured_content.as_ref().unwrap()["data"].clone();
    assert_eq!(reference["bytes"], 512 * 1024);
    let serialized = serde_json::to_string(&output).unwrap();
    assert!(serialized.len() < 2048);
    assert!(!serialized.contains("octet-transfer-"));
    assert!(!serialized.contains("locator"));
    let read = f
        .owned_call("measure", json!({"data": reference}))
        .await
        .unwrap();
    assert_eq!(
        read.structured_content,
        Some(json!({"bytes":512 * 1024,"sha256":reference["digest"]["value"]}))
    );
    assert!(f
        .process
        .call_tool(
            "measure",
            json!({"data":reference}),
            f.process
                .current_context_for_resource_owner("foreign-owner")
        )
        .await
        .is_err());
    let mut forged = reference.clone();
    forged["digest"]["value"] = json!("0".repeat(64));
    assert!(f
        .owned_call("measure", json!({"data":forged}))
        .await
        .is_err());
    assert!(
        !f.owned_call("measure", json!({"data":reference}))
            .await
            .unwrap()
            .is_error
    );
    assert!(f
        .log()
        .iter()
        .filter(|row| row["event"] == "read_closed")
        .all(|row| row["closed"] == true));
    f.shutdown().await;
}

#[tokio::test]
async fn b03_invalid_and_failed_parents_never_publish_committed_blob() {
    for name in ["invalid_output", "failed_parent"] {
        let f = Fixture::bulk().await;
        let output = f.owned_call(name, json!({"length":3})).await;
        if name == "invalid_output" {
            assert!(output.is_err());
        } else {
            let output = output.unwrap();
            assert!(output.is_error);
            assert!(output.structured_content.is_none());
        }
        let reference = f
            .log()
            .iter()
            .find(|row| row["event"] == "committed")
            .unwrap()["reference"]
            .clone();
        assert!(f
            .owned_call("measure", json!({"data":reference}))
            .await
            .is_err());
        f.shutdown().await;
    }
}

#[tokio::test]
async fn b06_cancellation_after_commit_never_publishes_blob() {
    let f = Fixture::bulk().await;
    let generation = f.process.health_snapshot().generation;
    let child = f.process.clone();
    let call = tokio::spawn(async move {
        child
            .call_tool(
                "cancel_publish",
                json!({"length":3}),
                child.current_context_for_resource_owner("python-test-owner"),
            )
            .await
    });
    f.barrier("entered").await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    f.barrier("cancelled").await;
    let reference = f
        .log()
        .iter()
        .find(|row| row["event"] == "committed")
        .unwrap()["reference"]
        .clone();
    assert!(f
        .owned_call("measure", json!({"data":reference}))
        .await
        .is_err());
    assert!(
        !f.owned_call("publish", json!({"length":3}))
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(f.process.health_snapshot().generation, generation);
    f.shutdown().await;
}

#[tokio::test]
async fn b07_cancelled_reader_closes_lease_without_revoking_retained_blob() {
    let f = Fixture::bulk().await;
    let generation = f.process.health_snapshot().generation;
    let output = f.owned_call("publish", json!({"length":3})).await.unwrap();
    let reference = output.structured_content.unwrap()["data"].clone();
    let child = f.process.clone();
    let arguments = json!({"data":reference});
    let call = tokio::spawn(async move {
        child
            .call_tool(
                "cancel_read",
                arguments,
                child.current_context_for_resource_owner("python-test-owner"),
            )
            .await
    });
    f.barrier("entered").await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    f.barrier("cancel_read_closed").await;
    assert_eq!(
        f.log()
            .iter()
            .find(|row| row["event"] == "cancel_read_closed")
            .unwrap()["closed"],
        true
    );
    assert!(
        !f.owned_call("measure", json!({"data":reference}))
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(f.process.health_snapshot().generation, generation);
    f.shutdown().await;
}

#[path = "support/resource_scope.rs"]
mod resource_scope;

#[path = "support/progress_unnegotiated.rs"]
mod progress_unnegotiated;

#[path = "support/values_matrix.rs"]
mod values_matrix;

#[path = "support/resource_matrix.rs"]
mod resource_matrix;

#[path = "support/bulk_matrix.rs"]
mod bulk_matrix;

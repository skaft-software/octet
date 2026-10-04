//! Source SDK acceptance through the production host, not a handwritten wire peer.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use octet_agent::{
    discover_extension_manifests, ExtensionCatalog, ExtensionEvent, ExtensionHealthState,
    ExtensionPolicy, ExtensionProcess, ExtensionRoot, ExtensionRuntimeConfig,
    ExtensionRuntimeError, ExtensionSource,
};
use serde_json::{json, Value};

static SERIAL: AtomicU64 = AtomicU64::new(0);
const NAME: &str = "typescript-typed-native";

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "octet-ts-native-{}-{nonce}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .expect("create owned private test directory");
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    // Drop the process before deleting its owned files, including on assertion failure.
    process: ExtensionProcess,
    directory: TestDir,
    log: PathBuf,
    home: PathBuf,
    generation: u64,
}
impl Fixture {
    async fn start() -> Self {
        assert!(
            cfg!(unix),
            "SDK CLI launcher requires Unix; missing support is not a skip"
        );
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .unwrap();
        let directory = TestDir::new();
        let home = directory.0.join("home");
        let workspace = directory.0.join("workspace");
        let root = directory.0.join("extensions");
        let extension_dir = root.join(NAME);
        for path in [&home, &workspace, &extension_dir] {
            fs::create_dir_all(path).unwrap();
        }
        let log = directory.0.join("calls.jsonl");
        fs::write(&log, "").unwrap();
        let manifest = extension_dir.join("extension.toml");
        let cli = repository.join("sdk/typescript/process/cli.mjs");
        let source = repository.join("sdk/typescript/tests/fixtures/typed-author.mjs");
        let generated = Command::new("node")
            .arg(&cli)
            .arg("manifest")
            .arg(&source)
            .args(["--name", NAME, "--version", "0.0.0", "--out"])
            .arg(&manifest)
            .current_dir(&workspace)
            .env("HOME", &home)
            .env("OCTET_TS_TYPED_LOG", &log)
            .output()
            .expect("Node >=22.19 required; do not silently skip");
        assert!(
            generated.status.success(),
            "source SDK CLI failed: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        assert!(
            fs::read_to_string(&log).unwrap().is_empty(),
            "manifest generation ran a handler"
        );
        // Keep the CLI-generated launcher/catalog. Only add per-child test environment,
        // as ordinary entrypoint metadata; never modify global HOME or author source.
        writeln!(
            OpenOptions::new().append(true).open(&manifest).unwrap(),
            "\n[entrypoint.env]\nHOME = {}\nOCTET_TS_TYPED_LOG = {}",
            serde_json::to_string(&home).unwrap(),
            serde_json::to_string(&log).unwrap()
        )
        .unwrap();
        let (inputs, diagnostics) = discover_extension_manifests(&[ExtensionRoot {
            directory: root,
            source: ExtensionSource::Explicit,
        }]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(inputs.len(), 1);
        let input = inputs.into_iter().next().unwrap();
        let off = ExtensionCatalog::load_resolved(
            [input.clone()],
            &ExtensionPolicy::default(),
            256 * 1024,
        );
        assert!(off.diagnostics.is_empty(), "{:?}", off.diagnostics);
        let mut config = ExtensionRuntimeConfig::new(&workspace);
        config.request_timeout = Duration::from_secs(5);
        config.cancellation_grace = Duration::from_secs(1);
        config.max_pending_requests = 1;
        config.supervise = false;
        assert!(matches!(
            ExtensionProcess::start(off.extensions.into_iter().next().unwrap(), config.clone())
                .await,
            Err(ExtensionRuntimeError::Disabled(_))
        ));
        let mut policy = ExtensionPolicy::default();
        policy.enable(NAME);
        let enabled = ExtensionCatalog::load_resolved([input], &policy, 256 * 1024);
        assert!(enabled.diagnostics.is_empty(), "{:?}", enabled.diagnostics);
        let process =
            ExtensionProcess::start(enabled.extensions.into_iter().next().unwrap(), config)
                .await
                .expect("source SDK initialize accepted by production ExtensionProcess");
        assert_eq!(process.api_version(), "0.4");
        assert_eq!(
            process
                .contributions()
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["typed_stats", "typed_null"]
        );
        let expected_features = ["content_parts", "request_cancellation", "request_progress"];
        let features = process.negotiated_features();
        assert_eq!(features.len(), expected_features.len());
        for feature in expected_features {
            assert!(features.contains(feature));
        }
        let generation = process.health_snapshot().generation;
        eprintln!(
            "SDK source={} CLI={} API=0.4 generation={generation}",
            source.display(),
            cli.display()
        );
        Self {
            process,
            directory,
            log,
            home,
            generation,
        }
    }

    fn records(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("complete append-only fixture log entry"))
            .collect()
    }

    fn count(&self, event: &str) -> usize {
        self.records()
            .iter()
            .filter(|row| row["event"] == event)
            .count()
    }

    async fn wait_for(&self, event: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            // Poll an explicit child-side barrier, not a guessed execution delay.
            while self.count(event) == 0 {
                assert!(self.process.is_running(), "child stopped before {event}");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("fixture never reached {event}: {:?}", self.records()));
    }

    async fn valid(&self, text: &str, characters: u64) {
        let output = self
            .process
            .call_tool(
                "typed_stats",
                json!({"text": text}),
                self.process.current_context(),
            )
            .await
            .expect("same process accepts a valid typed call");
        assert!(!output.is_error);
        assert_eq!(
            output.structured_content,
            Some(json!({"characters": characters, "note": null}))
        );
        assert_eq!(output.content, format!("{characters} Unicode characters"));
        assert_eq!(self.process.health_snapshot().generation, self.generation);
        assert_eq!(
            self.process.health_snapshot().state,
            ExtensionHealthState::Ready
        );
    }

    async fn close(&self) {
        assert!(
            self.process.shutdown().await,
            "host shutdown must acknowledge and exit cleanly"
        );
        assert_eq!(self.count("shutdown"), 1);
        let records = self.records();
        let pid = records
            .first()
            .expect("child log contains process identity")["pid"]
            .clone();
        assert!(pid.as_u64().is_some_and(|pid| pid > 0));
        assert!(
            records.iter().all(|row| row["pid"] == pid),
            "replacement child must not mask a failure"
        );
        for row in records.iter().filter(|row| row["event"] == "call") {
            assert_eq!(row["home"], json!(self.home));
        }
        assert_eq!(records.last().unwrap()["reason"], "shutdown");
        eprintln!(
            "SDK fixture evidence (temporary root={}):\n{}",
            self.directory.0.display(),
            fs::read_to_string(&self.log).unwrap()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_typed_roundtrip() {
    let fixture = Fixture::start().await;
    let definitions = fixture.process.tool_definitions();
    let stats = definitions
        .iter()
        .find(|tool| tool.name == "typed_stats")
        .unwrap();
    assert_eq!(
        stats.parameters,
        json!({"type": "object", "properties": {
        "text": {"type": "string"}, "mode": {"type": "string"}
    }, "additionalProperties": false})
    );
    assert_eq!(
        stats.output_schema,
        Some(json!({"type": "object", "properties": {
        "characters": {"type": "integer", "minimum": 0}, "note": {"type": "null"}
    }, "required": ["characters", "note"], "additionalProperties": false}))
    );
    fixture.valid("hé 🦀", 4).await;
    assert_eq!(fixture.count("call"), 1);
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_optional_and_explicit_null() {
    let fixture = Fixture::start().await;
    let omitted = fixture
        .process
        .call_tool("typed_stats", json!({}), fixture.process.current_context())
        .await
        .unwrap();
    assert_eq!(
        omitted.structured_content,
        Some(json!({"characters": 0, "note": null}))
    );
    assert_eq!(omitted.content, "0 Unicode characters");
    fixture.valid("present", 7).await;
    let null_schema = fixture
        .process
        .tool_definitions()
        .into_iter()
        .find(|tool| tool.name == "typed_null")
        .unwrap();
    assert_eq!(null_schema.output_schema, Some(json!({"type": "null"})));
    let null = fixture
        .process
        .call_tool("typed_null", json!({}), fixture.process.current_context())
        .await
        .unwrap();
    assert!(!null.is_error);
    assert_eq!(null.content, "No value");
    assert_eq!(
        null.structured_content,
        Some(Value::Null),
        "explicit null is not absent"
    );
    let serialized = serde_json::to_value(null).unwrap();
    assert!(serialized
        .as_object()
        .unwrap()
        .contains_key("structured_content"));
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_invalid_input_no_handler_entry() {
    let fixture = Fixture::start().await;
    for arguments in [
        json!({"text": 7}),
        json!({"text": null}),
        json!({"extra": true}),
        json!({"mode": 7}),
    ] {
        let before = fixture.count("call");
        let refused = fixture
            .process
            .call_tool("typed_stats", arguments, fixture.process.current_context())
            .await;
        assert!(
            matches!(
                refused,
                Err(ExtensionRuntimeError::Remote { code: -32602, .. })
            ),
            "{refused:?}"
        );
        assert_eq!(
            fixture.count("call"),
            before,
            "invalid input entered domain handler"
        );
    }
    fixture.valid("ok", 2).await;
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_invalid_output_recovery() {
    let fixture = Fixture::start().await;
    for mode in ["invalid", "nonfinite", "extra"] {
        let refused = fixture
            .process
            .call_tool(
                "typed_stats",
                json!({"mode": mode}),
                fixture.process.current_context(),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(ExtensionRuntimeError::Remote { code: -32603, .. })
            ),
            "{mode}: {refused:?}"
        );
        fixture.valid("recovered", 9).await;
    }
    let failure = fixture
        .process
        .call_tool(
            "typed_stats",
            json!({"mode": "error"}),
            fixture.process.current_context(),
        )
        .await
        .unwrap();
    assert!(failure.is_error);
    assert_eq!(failure.content, "Expected domain failure");
    assert!(
        failure.structured_content.is_none(),
        "domain failure need not invent an output value"
    );
    fixture.valid("still ready", 11).await;
    fixture.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn ts_native_cancellation_same_generation() {
    let fixture = Fixture::start().await;
    let mut events = fixture.process.subscribe();
    let mut call = Box::pin(fixture.process.call_tool(
        "typed_stats",
        json!({"mode": "cancel"}),
        fixture.process.current_context(),
    ));
    tokio::select! {
        _ = fixture.wait_for("entered") => {},
        result = &mut call => panic!("blocked handler completed before cancellation: {result:?}"),
    }
    assert_eq!(fixture.count("call"), 1);
    assert_eq!(fixture.count("returned"), 0);
    // The normal host waiter-drop path sends cancellation; the fixture never
    // receives a handwritten protocol frame from this test.
    drop(call);
    fixture.wait_for("cancelled").await;
    assert!(fixture
        .records()
        .iter()
        .any(|row| row["event"] == "cancelled" && row["aborted"] == true));
    let settled = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ExtensionEvent::Diagnostic { message } =
                events.recv().await.expect("host event stream")
            {
                assert!(
                    !message.starts_with("ignored response for unknown request"),
                    "duplicate terminal: {message}"
                );
                if message.starts_with("ignored late response for cancelled request ") {
                    break message;
                }
            }
        }
    })
    .await
    .expect("production host must observe cancellation terminal before grace escalation");
    assert_eq!(fixture.count("cancelled"), 1);
    assert_eq!(
        fixture.count("returned"),
        0,
        "cancelled call must not produce a successful typed result"
    );
    assert_eq!(fixture.process.health_snapshot().pending_requests, 0);
    fixture.valid("after cancel", 12).await;
    fixture.close().await;
    while let Ok(event) = events.try_recv() {
        if let ExtensionEvent::Diagnostic { message } = event {
            assert!(
                !message.starts_with("ignored response for unknown request"),
                "duplicate terminal: {message}"
            );
            assert_ne!(
                message, settled,
                "cancellation terminal must be observed once"
            );
        }
    }
}

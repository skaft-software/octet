//! Provider credential disclosure through a real extension process.
//!
//! The grant is review-time state, so the acceptance path starts at a real
//! manifest carrying `[capabilities] provider_credentials` and a real process
//! that asks the host for one exact provider/model over the live protocol. The
//! host resolves that identity against its own native catalog, which is the
//! only place a value can come from: the process never supplies one. A manifest
//! the reviewer did not opt in is refused during negotiation, another identity
//! never resolves to this value, and no host-visible summary carries it.

#[cfg(unix)]
use super::support::*;
#[cfg(unix)]
use super::*;

/// The endpoint credential the fixture process must never be able to infer,
/// name, or smuggle: only the host's own resolution may produce it.
#[cfg(unix)]
const FIXTURE_SECRET: &str = "scoped-fixture-secret";

#[cfg(unix)]
fn credential_manifest(declared: bool) -> ExtensionManifest {
    ExtensionManifest::parse(&format!(
        r#"
name = "credential-fixture"
version = "0.2.0"
api_version = "0.2"

[entrypoint]
command = "credential-fixture.sh"

[capabilities]
provider_credentials = {declared}

[contributes]
commands = ["credentials"]
"#
    ))
    .expect("credential fixture manifest")
}

/// A real catalog route for `test/test-model` whose only credential is the
/// fixture secret. Nothing else in the test may know it.
#[cfg(unix)]
fn credential_catalog() -> (ModelCatalog, Model) {
    let mut catalog = ModelCatalog::builtin().unwrap();
    let mut spec = (*catalog
        .resolve(&ModelId("gpt-6-astra".into()))
        .expect("builtin model")
        .spec)
        .clone();
    spec.id = ModelId("test/test-model".into());
    spec.endpoint = EndpointId("test-credentials".into());
    spec.api_name = "test-model".into();
    catalog
        .register_endpoint(Endpoint {
            id: spec.endpoint.clone(),
            base_url: url::Url::parse("https://example.test/v1/").unwrap(),
            auth: octet_ai::Auth::bearer(FIXTURE_SECRET),
            default_headers: http::HeaderMap::new(),
            transport: Default::default(),
            runtime: Default::default(),
            timeout: std::time::Duration::from_secs(30),
        })
        .unwrap();
    catalog.register_model(spec).unwrap();
    let model = catalog
        .resolve(&ModelId("test/test-model".into()))
        .expect("fixture model resolves");
    (catalog, model)
}

/// One fixture process that answers a `credentials` command by asking the host
/// for `provider`/`model` and writing the raw response where the test can read
/// it. It records the request it sent as well, so a refusal is proven by a real
/// exchange rather than by a missing file.
#[cfg(unix)]
fn credential_fixture(temp: &std::path::Path, provider: &str, model: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let response_path = temp.join(format!("credentials-{provider}-{model}.json"));
    let script = temp.join("credential-fixture.sh");
    let body = r#"#!/bin/sh
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
IFS= read -r initialize
id=$(printf '%s' "$initialize" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.2","tools":[],"commands":[{"name":"credentials","description":"Resolve one reviewed model credential","usage":"/credentials"}],"protocol":{"version":"0.2","features":["request_cancellation","content_parts","provider_credentials"],"limits":{"max_concurrent_requests":4}}}}\n' "$id"
IFS= read -r command
parent=$(printf '%s' "$command" | request_id)
printf '{"jsonrpc":"2.0","id":"pi:1","method":"provider/credentials","params":{"parent_request_id":%s,"provider":"@PROVIDER@","model":"@MODEL@"}}\n' "$parent"
IFS= read -r answer
printf '%s\n' "$answer" > '@RESPONSE@'
printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"credentials resolved","notifications":[],"context":[]}}\n' "$parent"
IFS= read -r shutdown
id=$(printf '%s' "$shutdown" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#
    .replace("@PROVIDER@", provider)
    .replace("@MODEL@", model)
    .replace("@RESPONSE@", &response_path.display().to_string());
    std::fs::write(&script, body).unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script, permissions).unwrap();
    response_path
}

#[cfg(unix)]
#[tokio::test]
async fn reviewed_provider_credentials_stay_scoped_to_one_model_and_never_leak() {
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let (catalog, current) = credential_catalog();
    let response_path = credential_fixture(temp.path(), "test", "test-model");

    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest: credential_manifest(true),
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("a reviewed credential fixture starts");
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some("foreground-owner".into());
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    extensions.refresh_pi_model_catalog(&catalog, None, &current);

    let mut confirmations = RecordingConfirmationHandler::default();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_command_with_confirmation("credentials", Vec::new(), &mut confirmations),
    )
    .await
    .expect("the reviewed credential request settled")
    .unwrap()
    .expect("the credential command produced output");
    assert_eq!(output, "credentials resolved");
    assert!(confirmations.calls.is_empty());

    // The exact requested route resolves to the host's own value.
    let answer: Value =
        serde_json::from_str(&std::fs::read_to_string(&response_path).unwrap()).unwrap();
    assert_eq!(answer["id"], "pi:1");
    assert_eq!(
        answer["result"],
        serde_json::json!({"ok": true, "apiKey": FIXTURE_SECRET, "headers": {}})
    );

    // Nothing the host reports about the extension carries the value.
    assert!(!output.contains(FIXTURE_SECRET));
    assert!(!extensions.inspect_text().contains(FIXTURE_SECRET));
    assert!(
        extensions
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains(FIXTURE_SECRET)),
        "{:?}",
        extensions.diagnostics.iter().collect::<Vec<_>>()
    );
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn another_model_never_resolves_through_a_reviewed_grant() {
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let (catalog, current) = credential_catalog();
    // The reviewed manifest is the same; only the asked-for identity differs,
    // and it names a provider the host has no credentials for.
    let response_path = credential_fixture(temp.path(), "other", "other-model");

    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest: credential_manifest(true),
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("a reviewed credential fixture starts");
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some("foreground-owner".into());
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    extensions.refresh_pi_model_catalog(&catalog, None, &current);

    let mut confirmations = RecordingConfirmationHandler::default();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        extensions.execute_command_with_confirmation("credentials", Vec::new(), &mut confirmations),
    )
    .await
    .expect("the redirecting credential request settled")
    .unwrap()
    .expect("the credential command produced output");
    assert_eq!(output, "credentials resolved");

    let answer: Value =
        serde_json::from_str(&std::fs::read_to_string(&response_path).unwrap()).unwrap();
    assert_eq!(answer["id"], "pi:1");
    assert_eq!(
        answer["result"],
        serde_json::json!({"ok": false, "error": "No API key found for \"other\""})
    );
    assert!(!answer.to_string().contains(FIXTURE_SECRET));
    assert!(!extensions.inspect_text().contains(FIXTURE_SECRET));
    assert!(
        extensions
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains(FIXTURE_SECRET)),
        "{:?}",
        extensions.diagnostics.iter().collect::<Vec<_>>()
    );
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreviewed_manifest_never_reaches_the_credential_path() {
    let temp = tempfile::tempdir().unwrap();
    credential_fixture(temp.path(), "test", "test-model");

    // The fixture echoes `provider_credentials`, but this manifest never
    // declared it: negotiation refuses the whole process instead of answering
    // a disclosure the reviewer never granted.
    let refused = match ExtensionProcess::start(
        DiscoveredExtension {
            manifest: credential_manifest(false),
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    {
        Ok(_) => panic!("an undeclared credential feature must fail negotiation"),
        Err(error) => error,
    };
    assert!(refused.to_string().contains("unknown feature"), "{refused}");
}

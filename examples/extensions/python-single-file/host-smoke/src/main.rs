use octet_agent::extension_process::{
    discover_extension_manifests, ExtensionCatalog, ExtensionExecutionContext, ExtensionHostState,
    ExtensionPolicy, ExtensionProcess, ExtensionRoot, ExtensionRuntimeConfig,
    ExtensionRuntimeError, ExtensionSource,
};
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() {
    let package = PathBuf::from(std::env::args_os().nth(1).expect("package directory"));
    let package = package.canonicalize().expect("package directory exists");
    let name = package
        .file_name()
        .and_then(|name| name.to_str())
        .expect("package directory has a UTF-8 extension name");
    let workspace = package
        .parent()
        .expect("package root has parent")
        .to_path_buf();
    let roots = [ExtensionRoot {
        directory: workspace.clone(),
        source: ExtensionSource::Explicit,
    }];
    let (inputs, diagnostics) = discover_extension_manifests(&roots);
    assert!(
        diagnostics.is_empty(),
        "discovery diagnostics: {diagnostics:?}"
    );
    let input = inputs
        .into_iter()
        .find(|input| input.path == package.join("extension.toml"))
        .expect("requested package discovered");
    let mut config = ExtensionRuntimeConfig::new(&workspace);
    config.request_timeout = Duration::from_secs(3);
    config.cancellation_grace = Duration::from_millis(300);
    config.supervise = false; // A restart must not disguise failed cancellation.
    let off =
        ExtensionCatalog::load_resolved([input.clone()], &ExtensionPolicy::default(), 256 * 1024);
    assert!(off.diagnostics.is_empty(), "{:?}", off.diagnostics);
    let descriptor = off.extensions.into_iter().next().expect("valid package");
    assert!(!descriptor.activation.enabled);
    assert!(matches!(
        ExtensionProcess::start(descriptor, config.clone()).await,
        Err(ExtensionRuntimeError::Disabled(_))
    ));
    println!("PASS discovery does not implicitly enable {name}");

    let mut policy = ExtensionPolicy::default();
    policy.enable(name);
    let catalog = ExtensionCatalog::load_resolved([input], &policy, 256 * 1024);
    assert!(
        catalog.diagnostics.is_empty(),
        "manifest diagnostics: {:?}",
        catalog.diagnostics
    );
    let descriptor = catalog
        .extensions
        .into_iter()
        .find(|entry| entry.manifest.name == name)
        .expect("generated package discovered");
    assert!(descriptor.activation.enabled);
    let process = ExtensionProcess::start(descriptor, config)
        .await
        .expect("real host starts and initializes extension");
    assert_eq!(process.api_version(), "0.4");
    assert!(process
        .negotiated_features()
        .contains("request_cancellation"));
    assert!(process.negotiated_features().contains("content_parts"));
    assert_eq!(process.tool_definitions().len(), 1);
    assert_eq!(process.tool_definitions()[0].name, "wait");
    let generation = process.health_snapshot().generation;
    println!("PASS explicit enablement and API 0.4 negotiation for {name}");

    let context = ExtensionExecutionContext {
        workspace: workspace.clone(),
        execution_scope: None,
        resource_owner: None,
        host: ExtensionHostState::default(),
    };
    let call = {
        let process = process.clone();
        tokio::spawn(async move {
            process
                .call_tool("wait", serde_json::json!({"steps": 1000}), context)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    call.abort(); // Dropping the host request sends the protocol cancellation notification.
    assert!(call.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(process.health_snapshot().generation, generation);
    assert!(process.is_running());

    let context = ExtensionExecutionContext {
        workspace,
        execution_scope: None,
        resource_owner: None,
        host: ExtensionHostState::default(),
    };
    let result = process
        .call_tool("wait", serde_json::json!({"steps": 1}), context)
        .await
        .expect("cancelled process remains responsive");
    assert_eq!(result.content, "Finished.");
    assert!(!result.is_error);
    assert_eq!(process.health_snapshot().generation, generation);
    println!("PASS cooperative cancellation and same-generation subsequent tool call");
    assert!(process.shutdown().await, "host acknowledged clean shutdown");
    assert!(!process.is_running());
    println!("PASS host-qualified shutdown");
}

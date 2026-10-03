use octet_agent::extension_process::{
    discover_extension_manifests, ExtensionCatalog, ExtensionExecutionContext, ExtensionHostState,
    ExtensionPolicy, ExtensionProcess, ExtensionRoot, ExtensionRuntimeConfig, ExtensionSource,
};
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() {
    let package = PathBuf::from(std::env::args_os().nth(1).expect("package directory"));
    let package = package.canonicalize().expect("package directory exists");
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
    let mut policy = ExtensionPolicy::default();
    policy.enable("wait-tool");
    let catalog = ExtensionCatalog::load_resolved(inputs, &policy, 256 * 1024);
    assert!(
        catalog.diagnostics.is_empty(),
        "manifest diagnostics: {:?}",
        catalog.diagnostics
    );
    let descriptor = catalog
        .extensions
        .into_iter()
        .find(|entry| entry.manifest.name == "wait-tool")
        .expect("generated package discovered");
    assert!(descriptor.activation.enabled);
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(&workspace))
        .await
        .expect("real host starts and initializes extension");

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
    tokio::time::sleep(Duration::from_millis(200)).await;

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
    assert!(process.shutdown().await, "host acknowledged clean shutdown");
    println!("host discovery, explicit enablement, API 0.4 initialize/tool/cancel/shutdown passed");
}

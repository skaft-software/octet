//! Read-only consumers for completed configuration and ingestion observations.
use super::*;
use octet_agent::SkillRegistry as _;

pub(super) fn known(resource: &str) -> bool {
    matches!(
        resource,
        "resource:settings" | "resource:mcp" | "resource:skills"
    )
}

/// Return content-free diagnostics only. Parsed values never replace the active
/// Config, trust policy, provider catalog, or model-visible skill registry.
pub(super) fn rescan(
    resource: &str,
    generation: u64,
    config: &Config,
    global_config: Option<&Path>,
) -> String {
    let result = match resource {
        "resource:settings" => global_config.ok_or(()).and_then(|path| {
            let bytes = octet_agent::secure_fs::read_regular_file_bounded(path, 1024 * 1024)
                .map_err(|_| ())?;
            let text = std::str::from_utf8(&bytes).map_err(|_| ())?;
            toml::from_str::<toml::Table>(text)
                .map(|_| ())
                .map_err(|_| ())
        }),
        "resource:mcp" => global_config
            .and_then(Path::parent)
            .ok_or(())
            .and_then(|root| {
                let bytes = octet_agent::secure_fs::read_private_file_bounded(
                    &root.join("mcp.json"),
                    256 * 1024,
                )
                .map_err(|_| ())?;
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
                if value.get("version").and_then(Value::as_u64) == Some(1)
                    && value.get("servers").is_some_and(Value::is_object)
                {
                    Ok(())
                } else {
                    Err(())
                }
            }),
        "resource:skills" => crate::resources::FileSystemSkillRegistry::new_with_invocation(
            config.workspace.clone(),
            config.invocation_cwd.clone(),
            config.skill_paths.clone(),
            config.workspace_trusted,
        )
        .map_err(|_| ())
        .and_then(|registry| {
            // This executes the ordinary bounded/no-follow discovery parser,
            // not a directory-count stand-in and not extension code.
            if registry.diagnostics().is_empty() {
                Ok(())
            } else {
                Err(())
            }
        }),
        _ => Err(()),
    };
    match result {
        Ok(()) => format!(
            "rescanned {resource} (generation {generation}); active configuration unchanged"
        ),
        Err(()) => format!(
            "warning: rescan of {resource} unavailable or invalid; active configuration unchanged"
        ),
    }
}

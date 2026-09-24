#![allow(missing_docs)]

//! The octet Herdr plugin: a startup hook that resumes recorded octet panes.
//!
//! Herdr cannot resume an agent kind it does not ship, so octet ships the
//! resume path itself. Herdr plugins are the documented extension surface for
//! exactly this ("Plugins turn that existing extension surface into reusable
//! workflows"), and a `[[startup]]` hook runs once after Herdr restores the
//! session and its API socket is ready — the only moment at which a restored
//! pane can be handed back to octet.
//!
//! The generated manifest is deliberately minimal: no events, no panes, no
//! actions, no build commands, and no state outside octet's own directories. It
//! runs the octet binary that installed it, as an argv list with no shell, and
//! the restore pass itself decides what to do (`crate::herdr::restore`).
//!
//! Installing is explicit (`octet herdr install-plugin`); nothing here runs
//! automatically, and the plugin can be removed with
//! `octet herdr uninstall-plugin`.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Globally unique plugin id inside Herdr's registry.
pub(crate) const PLUGIN_ID: &str = "octet.agent";
/// Oldest Herdr release whose plugin startup hooks and pane CLI this relies on.
pub(crate) const MIN_HERDR_VERSION: &str = "0.9.0";
const PLUGIN_DIR_NAME: &str = "herdr-plugin";
const MANIFEST_NAME: &str = "herdr-plugin.toml";

/// The plugin manifest written by the installer.
#[derive(Debug, Serialize)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    min_herdr_version: String,
    description: String,
    platforms: Vec<String>,
    startup: Vec<StartupHook>,
}

/// One `[[startup]]` hook.
#[derive(Debug, Serialize)]
struct StartupHook {
    command: Vec<String>,
}

/// Directory octet owns for the generated plugin.
pub(crate) fn plugin_dir() -> PathBuf {
    super::restore::octet_home().join(PLUGIN_DIR_NAME)
}

/// The generated manifest path.
fn manifest_path(directory: &Path) -> PathBuf {
    directory.join(MANIFEST_NAME)
}

/// Render the manifest for one octet executable.
///
/// `command` is an argv array, so an executable path containing spaces is
/// valid here; the restore pass independently refuses to place an unsafe path
/// inside a `herdr pane run` command string.
fn manifest_text(octet_executable: &Path, version: &str) -> Result<String, toml::ser::Error> {
    let manifest = Manifest {
        id: PLUGIN_ID.to_owned(),
        name: "octet".to_owned(),
        version: version.to_owned(),
        min_herdr_version: MIN_HERDR_VERSION.to_owned(),
        description: "Resume octet sessions in restored Herdr panes".to_owned(),
        // Windows is not declared until the restore pass is verified there.
        platforms: vec!["linux".to_owned(), "macos".to_owned()],
        startup: vec![StartupHook {
            command: vec![
                octet_executable.to_string_lossy().into_owned(),
                "herdr".to_owned(),
                "restore".to_owned(),
            ],
        }],
    };
    toml::to_string(&manifest)
}

/// The `herdr` binary to call for plugin management.
fn herdr_binary() -> PathBuf {
    match std::env::var("HERDR_BIN_PATH") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => PathBuf::from("herdr"),
    }
}

/// Run one `herdr` CLI call, inheriting stdio so the user sees its output.
fn run_herdr(args: &[String]) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new(herdr_binary())
        .args(args)
        .status()
}

/// Write the manifest and register it with Herdr.
pub(crate) fn install(octet_executable: &Path) -> anyhow::Result<()> {
    let directory = plugin_dir();
    std::fs::create_dir_all(&directory)
        .map_err(|error| anyhow::anyhow!("could not create {}: {error}", directory.display()))?;
    let manifest = manifest_text(octet_executable, env!("CARGO_PKG_VERSION"))
        .map_err(|error| anyhow::anyhow!("could not render the plugin manifest: {error}"))?;
    let path = manifest_path(&directory);
    std::fs::write(&path, manifest)
        .map_err(|error| anyhow::anyhow!("could not write {}: {error}", path.display()))?;
    crate::output::stdout_line(format!("Wrote {}", path.display()));
    let link = vec![
        "plugin".to_owned(),
        "link".to_owned(),
        directory.to_string_lossy().into_owned(),
        "--enabled".to_owned(),
    ];
    match run_herdr(&link) {
        Ok(status) if status.success() => {
            crate::output::stdout_line(format!(
                "Linked {PLUGIN_ID} with Herdr. It resumes recorded octet panes after a Herdr \
                 server restart."
            ));
            Ok(())
        }
        Ok(status) => anyhow::bail!(
            "Herdr refused to link the plugin (exit {}); the manifest is at {}",
            status.code().unwrap_or(-1),
            path.display()
        ),
        Err(error) => anyhow::bail!(
            "could not run the herdr CLI ({error}); the manifest is at {}. Link it manually with \
             `herdr plugin link {}`.",
            path.display(),
            directory.display()
        ),
    }
}

/// Unregister the plugin and remove octet's generated manifest.
pub(crate) fn uninstall() -> anyhow::Result<()> {
    let unlink = vec![
        "plugin".to_owned(),
        "unlink".to_owned(),
        PLUGIN_ID.to_owned(),
    ];
    match run_herdr(&unlink) {
        Ok(status) if status.success() => {
            crate::output::stdout_line(format!("Unlinked {PLUGIN_ID} from Herdr."));
        }
        Ok(status) => anyhow::bail!(
            "Herdr reported exit {} while unlinking {PLUGIN_ID}; the manifest was retained so the link can be retried",
            status.code().unwrap_or(-1)
        ),
        Err(error) => anyhow::bail!(
            "Could not run the herdr CLI ({error}); the manifest was retained so the link can be retried"
        ),
    }
    let directory = plugin_dir();
    if directory.exists() {
        std::fs::remove_dir_all(&directory).map_err(|error| {
            anyhow::anyhow!("could not remove {}: {error}", directory.display())
        })?;
        crate::output::stdout_line(format!("Removed {}", directory.display()));
    }
    Ok(())
}

/// Report the plugin, the generated manifest, and the recorded panes.
pub(crate) fn status() -> anyhow::Result<()> {
    let directory = plugin_dir();
    let path = manifest_path(&directory);
    crate::output::stdout_line(format!("plugin id: {PLUGIN_ID}"));
    crate::output::stdout_line(format!(
        "manifest:  {} ({})",
        path.display(),
        if path.exists() {
            "present"
        } else {
            "not installed"
        }
    ));
    crate::output::stdout_line("startup hook: <octet> herdr restore".to_owned());
    match run_herdr(&["plugin".to_owned(), "list".to_owned()]) {
        Ok(status) if status.success() => {}
        Ok(_) | Err(_) => crate::output::stdout_line(
            "Could not read the Herdr plugin list; run `herdr plugin list` directly.".to_owned(),
        ),
    }
    let records = super::restore::read_records(&super::restore::records_dir());
    crate::output::stdout_line(format!("recorded panes: {}", records.len()));
    for record in records {
        crate::output::stdout_line(format!(
            "  {} session {} cwd {}",
            record.pane_id, record.session_id, record.cwd
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_valid_toml_with_one_startup_hook() {
        let text = manifest_text(Path::new("/usr/local/bin/octet"), "0.8.0").unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["id"].as_str(), Some(PLUGIN_ID));
        assert_eq!(
            parsed["min_herdr_version"].as_str(),
            Some(MIN_HERDR_VERSION)
        );
        assert_eq!(parsed["version"].as_str(), Some("0.8.0"));
        let startup = parsed["startup"].as_array().unwrap();
        assert_eq!(startup.len(), 1);
        let command = startup[0]["command"].as_array().unwrap();
        assert_eq!(
            command
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["/usr/local/bin/octet", "herdr", "restore"]
        );
        // No events, actions, panes, or build commands: the hook is the whole
        // integration, so nothing else can run unexpectedly.
        for absent in ["events", "actions", "panes", "build", "link_handlers"] {
            assert!(
                parsed.get(absent).is_none(),
                "{absent} must not be declared"
            );
        }
        assert_eq!(
            parsed["platforms"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["linux", "macos"]
        );
    }

    #[test]
    fn a_path_with_spaces_stays_one_argv_element() {
        let text = manifest_text(Path::new("/Applications/My App/octet"), "0.8.0").unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        let command = parsed["startup"][0]["command"].as_array().unwrap();
        assert_eq!(command[0].as_str(), Some("/Applications/My App/octet"));
    }

    #[test]
    fn the_manifest_path_lives_in_octets_own_directory() {
        let directory = plugin_dir();
        assert!(directory.ends_with(PLUGIN_DIR_NAME));
        assert_eq!(
            manifest_path(&directory).file_name().unwrap(),
            MANIFEST_NAME
        );
    }
}

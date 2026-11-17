//! First-paint process boundary for the adapter's data-only chrome cache.
//! The adapter compiles Pi presentation. Rust only validates and paints its
//! bounded templates: starting Node here would already miss the first frame.
use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_BYTES: usize = 131072;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    schema: String,
    agent: PathBuf,
    pub(crate) workspace: PathBuf,
    trusted: bool,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    identity: Vec<Identity>,
    #[serde(default)]
    pub(crate) defaults: BTreeMap<String, String>,
    pub(crate) editor: Option<Editor>,
    pub(crate) footer: Option<Vec<Vec<Part>>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    path: PathBuf,
    hash: Option<String>,
    #[serde(default)]
    directory: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Editor {
    pub(crate) top: String,
    pub(crate) bottom: String,
    pub(crate) padding: usize,
    pub(crate) text_width: usize,
    pub(crate) max_rows: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum Part {
    Text { text: String },
    Field { field: Field },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Field {
    Model,
    ModelName,
    Provider,
    Reasoning,
    Workspace,
    HomeWorkspace,
    Zero,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path, max: usize) -> Option<Vec<u8>> {
    octet_agent::secure_fs::read_regular_file_bounded(path, max).ok()
}

fn safe_literal(text: &str, border: bool) -> bool {
    if text.contains(sexy_tui_rs::CURSOR_MARKER)
        || octet_agent::extension_remote_ui::validate_remote_ui_line(text).is_err()
    {
        return false;
    }
    let plain = sexy_tui_rs::strip_terminal_sequences(text);
    let mut remaining = plain.as_str();
    while !remaining.is_empty() {
        if !border {
            if let Some(word) = [
                "ctx", "context", "tokens", "total", "sub", "cost", "off", "minimal", "low",
                "medium", "high", "xhigh", "max", "auto", "Pi",
            ]
            .iter()
            .find(|word| remaining.starts_with(**word))
            {
                remaining = &remaining[word.len()..];
                continue;
            }
        }
        let ch = remaining.chars().next().unwrap();
        let structural = matches!(
            ch,
            ' ' | '─'
                | '━'
                | '│'
                | '┃'
                | '┌'
                | '┐'
                | '└'
                | '┘'
                | '╭'
                | '╮'
                | '╰'
                | '╯'
                | '═'
                | '║'
        );
        let footer = matches!(
            ch,
            '\u{e000}'
                ..='\u{f8ff}'
                    | '·'
                    | '|'
                    | '/'
                    | '\\'
                    | ':'
                    | ','
                    | '.'
                    | '%'
                    | '$'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '+'
                    | '-'
        );
        if !structural && (border || !footer) {
            return false;
        }
        remaining = &remaining[ch.len_utf8()..];
    }
    true
}

impl Snapshot {
    fn validated(self, agent: &Path, workspace: &Path, trusted: bool) -> Option<Self> {
        if self.schema != "octet.pi-chrome/v1"
            || self.agent != agent
            || self.workspace != workspace
            || self.trusted != trusted
            || !(3..=512).contains(&self.columns)
            || !(5..=256).contains(&self.rows)
            || self.identity.is_empty()
            || self.identity.len() > 512
        {
            return None;
        }
        if self.defaults.iter().any(|(field, value)| {
            !matches!(
                field.as_str(),
                "native_model" | "model" | "model_name" | "provider" | "reasoning"
            ) || value.len() > 256
                || value.chars().any(char::is_control)
        }) {
            return None;
        }
        for required in [
            agent.join("settings.json"),
            agent.join("npm/package-lock.json"),
        ] {
            if !self
                .identity
                .iter()
                .any(|entry| entry.path == required && !entry.directory)
            {
                return None;
            }
        }
        let mut total = 0;
        for identity in &self.identity {
            if !identity.path.is_absolute() || identity.path.as_os_str().len() > 4096 {
                return None;
            }
            // Identity inputs are hashes only: settings/package versions and
            // reviewed sources, never credentials or session data.
            let name = identity.path.file_name()?.to_str()?;
            if matches!(name, "auth.json" | "models.json" | "credentials.json") {
                return None;
            }
            let actual = if identity.directory {
                if std::fs::symlink_metadata(&identity.path).is_ok_and(|info| !info.is_dir()) {
                    return None;
                }
                match std::fs::read_dir(&identity.path) {
                    Ok(entries) => {
                        let mut names = entries
                            .take(4097)
                            .map(|entry| entry.ok()?.file_name().into_string().ok())
                            .collect::<Option<Vec<_>>>()?;
                        if names.len() > 4096 {
                            return None;
                        }
                        names.sort();
                        Some(digest(names.join("\n").as_bytes()))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(_) => return None,
                }
            } else {
                match read(&identity.path, 1024 * 1024) {
                    Some(bytes) => {
                        total += bytes.len();
                        Some(digest(&bytes))
                    }
                    None if !identity.path.exists() => None,
                    None => return None,
                }
            };
            if total > 8 * 1024 * 1024 || identity.hash != actual {
                return None;
            }
        }
        if let Some(editor) = &self.editor {
            if editor.padding > 8
                || editor.text_width == 0
                || editor.text_width > usize::from(self.columns).saturating_sub(editor.padding * 2)
                || !(1..=254).contains(&editor.max_rows)
                || editor.padding * 2 >= usize::from(self.columns)
                || !safe_literal(&editor.top, true)
                || !safe_literal(&editor.bottom, true)
                || sexy_tui_rs::visible_width(&editor.top) > usize::from(self.columns)
                || sexy_tui_rs::visible_width(&editor.bottom) > usize::from(self.columns)
            {
                return None;
            }
        }
        if let Some(footer) = &self.footer {
            if footer.len() > 8
                || footer.iter().any(|line| {
                    line.len() > 256
                        || line.iter().any(|part| match part {
                            Part::Text { text } => !safe_literal(text, false),
                            Part::Field { .. } => false,
                        })
                })
            {
                return None;
            }
        }
        (self.editor.is_some() || self.footer.is_some()).then_some(self)
    }
}

pub(crate) fn enabled(config: &Config) -> bool {
    config
        .enabled_extensions
        .iter()
        .any(|name| name == "octet-pi-compat")
        && config.start_extension_processes
        && config.sandbox.process_execution_allowed()
}

pub(crate) fn load(config: &Config) -> Option<Arc<Snapshot>> {
    if !enabled(config) {
        return None;
    }
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let discovered = resolver.discover(ResourceKind::Extension, &config.extension_paths);
    let mut diagnostics = Vec::new();
    let (policy, _) = trust_policy::extension_policy(config, &mut diagnostics);
    let descriptor = discovered
        .resources()
        .iter()
        .filter_map(|resource| {
            trust_policy::load_extension_descriptor(&resolver, resource, &policy, &mut diagnostics)
        })
        .find(|descriptor| descriptor.manifest.name == "octet-pi-compat")?;
    if !pi_mirror::needs_preparation(&descriptor, config) {
        return None;
    }
    let binding =
        pi_mirror::settings_binding(&descriptor, &config.workspace, config.workspace_trusted)?;
    let cache = descriptor
        .manifest
        .entrypoint
        .env
        .get("OCTET_PI_COMPAT_CACHE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("OCTET_PI_COMPAT_CACHE").map(PathBuf::from))
        .or_else(|| dirs::cache_dir().map(|path| path.join("octet-pi-compat")))?;
    let slot = digest(
        format!(
            "{}\n{}\n{}",
            binding.agent_dir.display(),
            config.workspace.display(),
            config.workspace_trusted
        )
        .as_bytes(),
    );
    let bytes = read(&cache.join(format!("chrome-{slot}.json")), MAX_BYTES)?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
    let value = snapshot.validated(
        &binding.agent_dir,
        &config.workspace,
        config.workspace_trusted,
    )?;
    if config.theme_explicit
        || config.model.as_ref().is_some_and(|model| {
            value
                .defaults
                .get("native_model")
                .is_some_and(|previous| previous != &model.0)
        })
    {
        return None;
    }
    crate::app::bootstrap::startup_phase("pi.chrome.snapshot-ready");
    Some(Arc::new(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot_value(root: &Path) -> serde_json::Value {
        let settings = root.join("settings.json");
        std::fs::write(&settings, br#"{"theme":"dark"}"#).unwrap();
        json!({
            "schema": "octet.pi-chrome/v1", "agent": root, "workspace": root,
            "trusted": false, "columns": 80, "rows": 24,
            "identity": [{"path": settings, "hash": digest(&std::fs::read(&settings).unwrap())},
                {"path": root.join("npm/package-lock.json"), "hash": null}],
            "editor": {"top": format!("\x1b[35m{}\x1b[39m", "─".repeat(80)), "bottom": "─".repeat(80), "padding": 0, "text_width": 79, "max_rows": 7},
            "footer": [[{"field":"model"}, {"text":" · "}, {"field":"zero"}]]
        })
    }

    fn snapshot(root: &Path) -> Snapshot {
        serde_json::from_value(snapshot_value(root)).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_cache_follows_native_activation_and_toggle_without_starting_node() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let extensions = root.join("extensions");
        let adapter = extensions.join("octet-pi-compat");
        let cache = root.join("cache");
        std::fs::create_dir_all(&adapter).unwrap();
        std::fs::create_dir(&cache).unwrap();
        let manifest = format!(
            "name = \"octet-pi-compat\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"node\"\n[entrypoint.env]\nOCTET_PI_AUTO_MIRROR = \"1\"\nOCTET_PI_AGENT_DIR = {agent}\nOCTET_PI_COMPAT_CACHE = {cache}\n",
            agent = serde_json::to_string(&root).unwrap(),
            cache = serde_json::to_string(&cache).unwrap(),
        );
        std::fs::write(adapter.join("extension.toml"), &manifest).unwrap();
        let file = cache.join(format!(
            "chrome-{}.json",
            digest(format!("{}\n{}\nfalse", root.display(), root.display()).as_bytes())
        ));
        std::fs::write(&file, serde_json::to_vec(&snapshot_value(&root)).unwrap()).unwrap();
        let mut config =
            super::super::tests::executable_extension_config(&root, &extensions, "octet-pi-compat");
        config.sandbox.allow_process = true;
        assert!(load(&config).is_some());
        let has_cached_border =
            |frame: Vec<String>| frame.iter().any(|line| line.contains("\x1b[35m─"));
        // Exercise the same native projection updates as setFooter(undefined):
        // keep geometry/model fixed, paint the live pair, then close the footer.
        for empty_footer in [false, true] {
            use crate::extensions::remote_ui::{MountView, Projection};
            use octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement as Placement;
            let mut shell = crate::tui::view::InteractiveShell::test_shell();
            shell.set_size(80, 24);
            shell.set_runtime_config(config.clone());
            shell.render();
            assert!(has_cached_border(
                shell.dump_rendered_frame().await.unwrap()
            ));
            let mut remote = Projection::default();
            for (placement, id, lines) in [
                (Placement::Editor, "editor", vec!["live editor".into()]),
                (
                    Placement::Footer,
                    "footer",
                    if empty_footer {
                        vec![]
                    } else {
                        vec!["live usage 42".into()]
                    },
                ),
                (Placement::BelowEditor, "widget", vec!["live widget".into()]),
            ] {
                remote.mounts.push(MountView {
                    id: id.into(),
                    surface_id: id.into(),
                    title: id.into(),
                    placement,
                    columns: 80,
                    rows: 24,
                    mouse_capture: false,
                    native_editor: false,
                });
                let components = Arc::make_mut(&mut remote.components);
                components.register_component(id).unwrap();
                components.store_render(id, 80, lines).unwrap();
            }
            shell.set_remote_ui(remote.clone());
            shell.render();
            let live = shell.dump_rendered_frame().await.unwrap().join("\n");
            assert!(
                live.contains("live editor"),
                "an empty frame is live: {live:?}"
            );
            assert!(live.contains("live widget"));
            assert!(!live.contains("\x1b[35m─"));
            assert_eq!(live.contains("live usage 42"), !empty_footer);
            remote
                .mounts
                .retain(|mount| mount.placement != Placement::Footer);
            Arc::make_mut(&mut remote.components).remove_component("footer");
            shell.set_remote_ui(remote);
            shell.render();
            let cleared = shell.dump_rendered_frame().await.unwrap().join("\n");
            assert!(
                cleared.contains("live editor"),
                "clearing the footer must not resurrect cached chrome: {cleared:?}"
            );
            assert!(cleared.contains("live widget"));
            assert!(!cleared.contains("\x1b[35m─"));
            // An ordinary preference/config refresh is not a new activation.
            shell.set_runtime_config(config.clone());
            shell.render();
            assert!(!has_cached_border(
                shell.dump_rendered_frame().await.unwrap()
            ));
            let mut disabled = config.clone();
            disabled.enabled_extensions.clear();
            shell.set_runtime_config(disabled);
            shell.set_remote_ui(Default::default());
            shell.set_runtime_config(config.clone());
            shell.render();
            assert!(has_cached_border(
                shell.dump_rendered_frame().await.unwrap()
            ));
        }
        let mut shell = crate::tui::view::InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.set_runtime_config(config.clone());
        shell.render();
        assert!(has_cached_border(
            shell.dump_rendered_frame().await.unwrap()
        ));
        let mut failed = ExecutableExtensions::default();
        failed
            .start_failures
            .insert("octet-pi-compat".into(), "fixture startup failure".into());
        failed.sync_remote_ui(&mut shell);
        assert!(!has_cached_border(
            shell.dump_rendered_frame().await.unwrap()
        ));
        shell.set_runtime_config(config.clone());
        shell.render();
        assert!(!has_cached_border(
            shell.dump_rendered_frame().await.unwrap()
        ));
        let mut shell = crate::tui::view::InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.extension_set_editor("retained draft".into());
        shell.set_runtime_config(config.clone());
        shell.render();
        assert!(has_cached_border(
            shell.dump_rendered_frame().await.unwrap()
        ));
        config.enabled_extensions.clear();
        shell.set_runtime_config(config.clone());
        shell.render();
        assert!(!has_cached_border(
            shell.dump_rendered_frame().await.unwrap()
        ));
        config.enabled_extensions.push("octet-pi-compat".into());
        shell.set_runtime_config(config.clone());
        shell.render();
        assert!(has_cached_border(
            shell.dump_rendered_frame().await.unwrap()
        ));
        assert_eq!(shell.pending(), "retained draft");
        config.theme_explicit = true;
        assert!(load(&config).is_none());
        config.theme_explicit = false;
        config.start_extension_processes = false;
        assert!(load(&config).is_none());
        config.start_extension_processes = true;
        std::fs::write(
            adapter.join("extension.toml"),
            manifest.replace(
                "OCTET_PI_AUTO_MIRROR = \"1\"",
                "OCTET_PI_AUTO_MIRROR = \"0\"",
            ),
        )
        .unwrap();
        assert!(load(&config).is_none(), "an explicitly configured, non-mirrored adapter cannot reuse an automatic mirror's chrome");
        std::fs::write(adapter.join("extension.toml"), manifest).unwrap();
        std::fs::write(file, "corrupt cache").unwrap();
        assert!(load(&config).is_none());
    }

    #[test]
    fn snapshot_invalidates_changed_settings_versions_identity_and_schema() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let value = snapshot(root);
        assert!(value.clone().validated(root, root, false).is_some());
        assert!(value.clone().validated(root, root, true).is_none());
        assert!(value
            .clone()
            .validated(root, &root.join("other"), false)
            .is_none());
        let mut next = value.clone();
        next.schema.push('2');
        assert!(next.validated(root, root, false).is_none());
        let mut next = value.clone();
        next.editor.as_mut().unwrap().padding = usize::MAX;
        assert!(next.validated(root, root, false).is_none());
        std::fs::write(root.join("settings.json"), br#"{"theme":"light"}"#).unwrap();
        assert!(value.validated(root, root, false).is_none());
        let value = snapshot(root);
        std::fs::create_dir(root.join("npm")).unwrap();
        std::fs::write(root.join("npm/package-lock.json"), "new version").unwrap();
        assert!(value.validated(root, root, false).is_none());
    }

    #[test]
    fn snapshot_invalidates_directory_membership_and_rejects_opaque_content() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut value = snapshot(root);
        std::fs::create_dir(root.join("extensions")).unwrap();
        value.identity.push(Identity {
            path: root.join("extensions"),
            hash: Some(digest(b"")),
            directory: true,
        });
        assert!(value.clone().validated(root, root, false).is_some());
        std::fs::write(root.join("extensions/new.mjs"), "").unwrap();
        assert!(value.validated(root, root, false).is_none());
        for unsafe_text in [
            "private-session-title",
            "sk-secret",
            "\x1b]0;title\x07",
            sexy_tui_rs::CURSOR_MARKER,
            "42 tokens",
        ] {
            let mut value = snapshot(root);
            value.footer = Some(vec![vec![Part::Text {
                text: unsafe_text.into(),
            }]]);
            assert!(
                value.validated(root, root, false).is_none(),
                "{unsafe_text:?}"
            );
        }
    }
}

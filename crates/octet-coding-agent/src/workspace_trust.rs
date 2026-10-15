//! Persistent user-owned decisions for loading project-provided workspace resources.
//!
//! Workspace trust admits project configuration, instructions and resources. It
//! does not itself grant tool-effect permissions, extension host authority, or sandboxing.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Context as _;

const MAX_TRUST_STORE_BYTES: usize = 1024 * 1024;
const MAX_TRUST_PATH_BYTES: usize = 8 * 1024;

/// Fallback policy when no path-specific workspace trust decision exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DefaultWorkspaceTrust {
    /// Ask on eligible interactive launches; otherwise leave unmatched paths untrusted.
    #[default]
    Ask,
    /// Trust every workspace unless an explicit path decision denies it.
    Always,
    /// Do not trust workspaces unless an explicit path decision allows them.
    Never,
}

impl DefaultWorkspaceTrust {
    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ask" => Ok(Self::Ask),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            _ => anyhow::bail!(
                "invalid default_workspace_trust {value:?}; use ask, always, or never"
            ),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// One saved path decision, returned with the path whose decision matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrustRecord {
    pub(crate) trusted: bool,
    pub(crate) path: PathBuf,
}

/// Startup answer: persist the decision or use it for this launch only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupChoice {
    Remember(bool),
    Session(bool),
}

/// Parse the numbered startup trust choice. Empty input safely means no trust.
pub(crate) fn parse_startup_choice(input: &str) -> Option<StartupChoice> {
    match input.trim() {
        "1" => Some(StartupChoice::Remember(true)),
        "2" => Some(StartupChoice::Session(true)),
        "3" => Some(StartupChoice::Remember(false)),
        "4" | "" => Some(StartupChoice::Session(false)),
        _ => None,
    }
}

/// User-owned, path-based decisions in `~/.octet/trust.json`.
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceTrustStore {
    path: PathBuf,
}

impl WorkspaceTrustStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The store sits beside the user configuration, never in a project.
    pub(crate) fn path_for_config(config_path: &Path) -> PathBuf {
        config_path.with_file_name("trust.json")
    }

    /// Resolve the closest saved decision at the workspace or one of its parents.
    pub(crate) fn decision_for(&self, workspace: &Path) -> anyhow::Result<Option<TrustRecord>> {
        let workspace = workspace.canonicalize().with_context(|| {
            format!(
                "cannot resolve workspace trust path {}",
                workspace.display()
            )
        })?;
        let records = self.read_records()?;
        for ancestor in workspace.ancestors() {
            let Some(key) = trust_key(ancestor) else {
                continue;
            };
            if let Some(trusted) = records.get(&key) {
                return Ok(Some(TrustRecord {
                    trusted: *trusted,
                    path: ancestor.to_owned(),
                }));
            }
        }
        Ok(None)
    }

    /// Atomically apply one or more path decisions under the store's private lock.
    /// `None` removes a workspace override so it inherits from its parent/default.
    pub(crate) fn update(&self, changes: &[(PathBuf, Option<bool>)]) -> anyhow::Result<()> {
        let normalized = changes
            .iter()
            .map(|(path, decision)| {
                let canonical = path.canonicalize().with_context(|| {
                    format!("cannot resolve workspace trust path {}", path.display())
                })?;
                let key = trust_key(&canonical).ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot persist workspace trust for non-UTF-8 path {}",
                        canonical.display()
                    )
                })?;
                Ok((key, *decision))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        self.with_lock(|| {
            let (mut records, original) = self.read_records_with_bytes()?;
            for (path, decision) in normalized {
                if let Some(trusted) = decision {
                    records.insert(path, trusted);
                } else {
                    records.remove(&path);
                }
            }
            let bytes = serde_json::to_vec_pretty(&records)?;
            octet_agent::secure_fs::write_private_atomic_if_unchanged(
                &self.path,
                original.as_deref(),
                &bytes,
                MAX_TRUST_STORE_BYTES,
            )
            .with_context(|| {
                format!(
                    "could not update workspace trust store {}",
                    self.path.display()
                )
            })?;
            Ok(())
        })
    }

    fn read_records(&self) -> anyhow::Result<BTreeMap<String, bool>> {
        self.read_records_with_bytes().map(|(records, _)| records)
    }

    fn read_records_with_bytes(&self) -> anyhow::Result<(BTreeMap<String, bool>, Option<Vec<u8>>)> {
        let bytes = match octet_agent::secure_fs::read_private_file_bounded(
            &self.path,
            MAX_TRUST_STORE_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(octet_agent::secure_fs::SecureFileError::Io(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                return Ok((BTreeMap::new(), None))
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "could not read workspace trust store {}",
                        self.path.display()
                    )
                })
            }
        };
        let records: BTreeMap<String, bool> = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid workspace trust store {}", self.path.display()))?;
        for path in records.keys() {
            if path.len() > MAX_TRUST_PATH_BYTES || !Path::new(path).is_absolute() {
                anyhow::bail!(
                    "invalid workspace path in trust store {}",
                    self.path.display()
                );
            }
        }
        Ok((records, Some(bytes)))
    }

    fn with_lock<T>(&self, operation: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        use fs2::FileExt as _;

        let lock_path = self.path.with_file_name("trust.json.lock");
        let lock_file =
            octet_agent::secure_fs::open_private_lock_file(&lock_path).with_context(|| {
                format!(
                    "could not open workspace trust lock {}",
                    lock_path.display()
                )
            })?;
        lock_file.lock_exclusive().with_context(|| {
            format!(
                "could not lock workspace trust store {}",
                self.path.display()
            )
        })?;
        let identity = match octet_agent::secure_fs::validate_private_lock_after_acquire(
            &lock_path, &lock_file,
        ) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = fs2::FileExt::unlock(&lock_file);
                return Err(error).context("could not validate workspace trust lock");
            }
        };

        let result = operation();
        let validation = octet_agent::secure_fs::revalidate_private_lock_before_release(
            &lock_path, &lock_file, &identity,
        )
        .context("workspace trust lock changed before release");
        let unlock =
            fs2::FileExt::unlock(&lock_file).context("could not release workspace trust lock");
        match (result, validation, unlock) {
            (Err(error), _, _) => Err(error),
            (Ok(_), Err(error), _) | (Ok(_), Ok(()), Err(error)) => Err(error),
            (Ok(value), Ok(()), Ok(())) => Ok(value),
        }
    }
}

fn trust_key(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    (path.is_absolute() && text.len() <= MAX_TRUST_PATH_BYTES).then(|| text.to_owned())
}

/// Whether this workspace contains resources that the trust gate would admit.
pub(crate) fn has_project_resources(
    workspace: &Path,
    invocation_cwd: &Path,
    global_config: Option<&Path>,
) -> bool {
    let project_octet = workspace.join(".octet");
    let global_octet = global_config.and_then(Path::parent);
    let project_is_global =
        global_octet.is_some_and(|global| same_directory(&project_octet, global));

    if !project_is_global {
        if project_octet.join("config.toml").exists()
            || ["extensions", "prompts", "skills", "themes"]
                .iter()
                .any(|name| project_octet.join(name).exists())
        {
            return true;
        }
    }

    for directory in crate::resources::dirs_from_workspace_to_cwd(workspace, invocation_cwd) {
        if directory.join("AGENTS.md").exists() {
            return true;
        }
        let project_skills = directory.join(".agents/skills");
        let user_agents_skills = global_octet
            .and_then(Path::parent)
            .map(|home| home.join(".agents/skills"));
        if project_skills.exists()
            && !user_agents_skills
                .as_deref()
                .is_some_and(|user| same_directory(&project_skills, user))
        {
            return true;
        }
    }

    invocation_cwd.join(".pi/skills").exists()
}

fn same_directory(left: &Path, right: &Path) -> bool {
    left.canonicalize().unwrap_or_else(|_| left.to_owned())
        == right.canonicalize().unwrap_or_else(|_| right.to_owned())
}

/// Ask before loading project resources. The menu is printed before terminal raw
/// mode begins, and a blank/EOF response safely chooses session-only denial.
pub(crate) fn prompt_for_workspace(workspace: &Path) -> anyhow::Result<StartupChoice> {
    crate::output::stderr_line(format!(
        "Project-controlled resources were found in {}.",
        workspace.display()
    ));
    crate::output::stderr_line(
        "Workspace trust admits project config and resources; it does not itself grant tool permissions or extension host authority.",
    );
    crate::output::stderr_line(
        "Trusted project config may enable extensions; extension startup follows the separate host-authority policy.",
    );
    crate::output::stderr_line(
        "[1] Trust this workspace and remember it  [2] Trust for this launch only",
    );
    crate::output::stderr_line(
        "[3] Do not trust this workspace and remember  [4] Continue without trust (default)",
    );

    loop {
        eprint!("Workspace trust [4]: ");
        io::stderr().flush()?;
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer)? == 0 {
            return Ok(StartupChoice::Session(false));
        }
        if let Some(choice) = parse_startup_choice(&answer) {
            return Ok(choice);
        }
        crate::output::stderr_line("Choose 1, 2, 3, or 4.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_saved_decision_wins_and_can_be_removed_to_inherit() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("repo").join("nested");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = WorkspaceTrustStore::new(root.path().join("octet/trust.json"));
        store
            .update(&[
                (root.path().to_path_buf(), Some(true)),
                (workspace.clone(), Some(false)),
            ])
            .unwrap();
        assert_eq!(
            store.decision_for(&workspace).unwrap().unwrap().trusted,
            false
        );
        store.update(&[(workspace.clone(), None)]).unwrap();
        let inherited = store.decision_for(&workspace).unwrap().unwrap();
        assert!(inherited.trusted);
        assert_eq!(inherited.path, root.path().canonicalize().unwrap());
    }

    #[test]
    fn updating_store_is_private_and_rejects_malformed_or_symlink_store() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("repo");
        std::fs::create_dir(&workspace).unwrap();
        let store = WorkspaceTrustStore::new(root.path().join("octet/trust.json"));
        store.update(&[(workspace.clone(), Some(true))]).unwrap();
        let bytes = octet_agent::secure_fs::read_private_file_bounded(&store.path, 1024).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let records: BTreeMap<String, bool> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            records.get(workspace.canonicalize().unwrap().to_str().unwrap()),
            Some(&true)
        );

        std::fs::write(&store.path, b"not json").unwrap();
        assert!(store.decision_for(&workspace).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let external = root.path().join("external.json");
            std::fs::write(&external, b"{}").unwrap();
            std::fs::remove_file(&store.path).unwrap();
            symlink(&external, &store.path).unwrap();
            assert!(store.decision_for(&workspace).is_err());
        }
    }

    #[test]
    fn startup_choices_default_to_untrusted_and_distinguish_persistence() {
        assert_eq!(
            parse_startup_choice("1"),
            Some(StartupChoice::Remember(true))
        );
        assert_eq!(
            parse_startup_choice("2"),
            Some(StartupChoice::Session(true))
        );
        assert_eq!(
            parse_startup_choice("3"),
            Some(StartupChoice::Remember(false))
        );
        assert_eq!(
            parse_startup_choice("4"),
            Some(StartupChoice::Session(false))
        );
        assert_eq!(
            parse_startup_choice(""),
            Some(StartupChoice::Session(false))
        );
        assert_eq!(parse_startup_choice("y"), None);
    }

    #[test]
    fn project_detection_ignores_global_resources_but_finds_project_resources() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(home.join(".octet/skills")).unwrap();
        std::fs::create_dir_all(home.join(".agents/skills")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let global_config = home.join(".octet/config.toml");
        assert!(!has_project_resources(&home, &home, Some(&global_config)));
        assert!(!has_project_resources(
            &workspace,
            &workspace,
            Some(&global_config)
        ));
        std::fs::create_dir_all(workspace.join(".octet/prompts")).unwrap();
        assert!(has_project_resources(
            &workspace,
            &workspace,
            Some(&global_config)
        ));
    }
}

#![allow(missing_docs)]

//! Typed, read-only Pi adapter and host-owned migration ingestion.
//!
//! The adapter process is deliberately unable to persist anything: it receives
//! one source root over API 0.3 and returns bounded non-secret values. This
//! module owns all destination reads, conflict decisions, backups, and writes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::IsTerminal as _;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt as _;
use octet_agent::extension_api_v03 as api;
use octet_ai::ModelCatalog;
use octet_migrate_types::{
    Diagnostic as SetupDiagnostic, DiagnosticSeverity, McpServer, McpTransport, MigratedSetup,
    MigrationOutcome, Model, Skill,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use super::{absolute_path, MigrationAdapterCommand, MigrationImportCommand};

mod pi_adapter;

use pi_adapter::{run_pi_adapter_stdio, AdapterClient};
// The suite pins the adapter's own wire and source readers directly. Keeping
// them reachable through this module's glob means the test file is unchanged by
// the split, and gating the import keeps the library build warning-free.
#[cfg(test)]
use pi_adapter::{parse_canonical_adapter_frame, pi_detect, pi_import, read_bounded_adapter_line};
// `BufReader` moved with the adapter's bounded frame reader; the suite still
// drives that reader over a slice, so it needs the type back here.
#[cfg(test)]
use std::io::BufReader;

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_MCP_CONFIG_BYTES: usize = 256 * 1024;
const MAX_SKILL_BYTES: usize = 128 * 1024;
const MAX_STATE_BYTES: usize = 256 * 1024;
const MAX_BACKUP_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_SOURCE_ITEMS: usize = 128;
const MIGRATION_STATE_VERSION: u32 = 1;
const BACKUP_VERSION: u32 = 1;
const UNKNOWN_MODEL_DIAGNOSTIC: &str =
    "Configured Pi model was skipped because no exact built-in octet provider/API-name match was found.";
const AMBIGUOUS_MODEL_DIAGNOSTIC: &str =
    "Configured Pi model was skipped because its built-in octet provider/API-name match is ambiguous.";

/// Dispatch the public `octet migrate import ...` command.
pub(crate) fn run_import(
    command: MigrationImportCommand,
    invocation_cwd: &Path,
) -> anyhow::Result<()> {
    match command {
        MigrationImportCommand::Pi {
            source,
            yes,
            dry_run,
            json,
        } => run_import_pi(source, yes, dry_run, json, invocation_cwd),
    }
}

/// Dispatch the intentionally hidden, current-binary Pi adapter entrypoint.
pub(crate) fn run_adapter(command: MigrationAdapterCommand) -> anyhow::Result<()> {
    match command {
        MigrationAdapterCommand::Pi => run_pi_adapter_stdio(),
    }
}

/// Restore a migration backup after checking that the destination still matches
/// the import it backs up. `force` is an explicit user override for changed
/// destinations.
pub(crate) fn run_restore(
    backup: PathBuf,
    force: bool,
    invocation_cwd: &Path,
) -> anyhow::Result<()> {
    let home = migration_home()?;
    let backup = absolute_path(&backup, invocation_cwd)?;
    let paths = MigrationPaths::new(home)?;
    let lock = MigrationLock::acquire(&paths)?;
    let restored = restore_backup(&paths, &backup, force)?;
    lock.release()?;
    crate::output::stdout_line(format!(
        "Restored {restored} migration target(s) from {}.",
        backup.display()
    ));
    Ok(())
}

fn run_import_pi(
    source: Option<PathBuf>,
    yes: bool,
    dry_run: bool,
    json_output: bool,
    invocation_cwd: &Path,
) -> anyhow::Result<()> {
    let home = migration_home()?;
    let explicit_source = source.is_some();
    // Preserve the selected source leaf until the read-only adapter validates
    // it. Canonicalizing here would hide a symlink from its no-follow check.
    let source = match source {
        Some(source) => invocation_cwd.join(source),
        None => default_pi_source(&home)?,
    };
    if !source.is_dir() {
        if explicit_source {
            anyhow::bail!("Pi source directory does not exist: {}", source.display())
        }
        emit_no_source_report(&source, json_output);
        return Ok(());
    }
    let paths = MigrationPaths::new(home)?;
    // A preview must not create destination state. Real imports still acquire
    // this before source or destination planning, so their conflict checks and
    // writes remain serialized.
    let lock = if dry_run {
        None
    } else {
        Some(MigrationLock::acquire(&paths)?)
    };

    let mut adapter = AdapterClient::start()?;
    let detected = adapter.detect(&source)?;
    if !detected.detected {
        adapter.shutdown();
        if let Some(lock) = lock {
            lock.release()?;
        }
        emit_no_source_report(&source, json_output);
        return Ok(());
    }
    let imported = adapter.import(&source, &detected.config_paths)?;
    adapter.shutdown();
    let setup = normalize_adapter_result(imported)?;

    let preview = build_ingestion_plan(&paths, &setup, false)?;
    if dry_run {
        emit_import_report(&source, &preview, None, true, json_output);
        return Ok(());
    }
    let lock = lock.expect("mutating imports acquire the migration lock");
    if !preview.conflicts.is_empty() && !confirm_conflicts(&preview.conflicts, yes)? {
        lock.release()?;
        anyhow::bail!("migration cancelled; no files were changed")
    }

    let plan = if preview.conflicts.is_empty() {
        preview
    } else {
        build_ingestion_plan(&paths, &setup, true)?
    };
    let backup = apply_ingestion_plan(&paths, &plan)?;
    lock.release()?;
    emit_import_report(&source, &plan, backup.as_deref(), false, json_output);
    Ok(())
}

fn migration_home() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home directory is unavailable"))?;
    let current = std::env::current_dir()?;
    absolute_path(&home, &current)
}

fn default_pi_source(home: &Path) -> anyhow::Result<PathBuf> {
    if let Some(value) = std::env::var_os("PI_CODING_AGENT_DIR") {
        return Ok(std::env::current_dir()?.join(value));
    }

    let mut candidates = vec![home.join(".pi/agent"), home.join(".config/pi/agent")];
    #[cfg(target_os = "macos")]
    candidates.push(home.join("Library/Application Support/pi/agent"));
    let selected = candidates
        .iter()
        .find(|candidate| candidate.is_dir())
        .cloned()
        .unwrap_or_else(|| candidates.remove(0));
    Ok(selected)
}

#[derive(Clone, Debug)]
struct MigrationPaths {
    home: PathBuf,
    config: PathBuf,
    mcp: PathBuf,
    skills: PathBuf,
    state: PathBuf,
    lock: PathBuf,
    backups: PathBuf,
}

impl MigrationPaths {
    fn new(home: PathBuf) -> anyhow::Result<Self> {
        if !home.is_absolute() {
            anyhow::bail!("migration home must be absolute")
        }
        let octet = home.join(".octet");
        Ok(Self {
            config: octet.join("config.toml"),
            mcp: octet.join("mcp.json"),
            skills: octet.join("skills"),
            state: octet.join("migrations/pi-state.json"),
            lock: octet.join("migrations/pi-import.lock"),
            backups: octet.join("backups/migrate"),
            home,
        })
    }
}

struct MigrationLock {
    path: PathBuf,
    file: fs::File,
    identity: octet_agent::secure_fs::PrivateLockIdentity,
}

impl MigrationLock {
    fn acquire(paths: &MigrationPaths) -> anyhow::Result<Self> {
        let file =
            octet_agent::secure_fs::open_private_lock_file(&paths.lock).map_err(|error| {
                anyhow::anyhow!(
                    "cannot create the private migration lock {}: {error}",
                    paths.lock.display()
                )
            })?;
        file.try_lock_exclusive().map_err(|error| {
            anyhow::anyhow!("another migration is already updating this octet home: {error}")
        })?;
        let identity =
            octet_agent::secure_fs::validate_private_lock_after_acquire(&paths.lock, &file)
                .map_err(|error| anyhow::anyhow!("cannot validate migration lock: {error}"))?;
        Ok(Self {
            path: paths.lock.clone(),
            file,
            identity,
        })
    }

    fn release(self) -> anyhow::Result<()> {
        octet_agent::secure_fs::revalidate_private_lock_before_release(
            &self.path,
            &self.file,
            &self.identity,
        )
        .map_err(|error| anyhow::anyhow!("migration lock changed while held: {error}"))?;
        fs2::FileExt::unlock(&self.file)?;
        Ok(())
    }
}

fn normalize_adapter_result(result: api::MigrationImportResult) -> anyhow::Result<MigratedSetup> {
    let mut models = Vec::new();
    let mut skills = Vec::new();
    let mut servers = Vec::new();
    for model in result.models {
        models.push(MigrationOutcome::mapped(
            model.path,
            Model::new(model.provider, model.model)
                .map_err(|error| anyhow::anyhow!("adapter model is not migration-safe: {error}"))?,
        )?);
    }
    for skill in result.skills {
        skills.push(MigrationOutcome::mapped(
            skill.path,
            Skill::new(skill.name, skill.content)
                .map_err(|error| anyhow::anyhow!("adapter skill is not migration-safe: {error}"))?,
        )?);
    }
    for server in result.mcp_servers {
        let transport = McpTransport::stdio(server.command, server.args).map_err(|error| {
            anyhow::anyhow!("adapter MCP server is not migration-safe: {error}")
        })?;
        servers.push(MigrationOutcome::mapped(
            server.path,
            McpServer::new(server.name, transport).map_err(|error| {
                anyhow::anyhow!("adapter MCP server is not migration-safe: {error}")
            })?,
        )?);
    }
    let diagnostics = result
        .diagnostics
        .into_iter()
        .map(|diagnostic| {
            let severity = match diagnostic.severity.as_str() {
                "warning" => DiagnosticSeverity::Warning,
                "error" => DiagnosticSeverity::Error,
                _ => anyhow::bail!("adapter supplied an unknown diagnostic severity"),
            };
            SetupDiagnostic::new(diagnostic.path, severity, diagnostic.reason).map_err(|error| {
                anyhow::anyhow!("adapter diagnostic is not migration-safe: {error}")
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    MigratedSetup::with_parts("pi", models, skills, servers, Vec::new(), diagnostics)
        .map_err(|error| anyhow::anyhow!("adapter output exceeds migration schema bounds: {error}"))
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PiMigrationState {
    version: u32,
    #[serde(default)]
    skills: BTreeMap<String, StateEntry>,
    #[serde(default)]
    mcp_servers: BTreeMap<String, StateEntry>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StateEntry {
    hash: String,
}

impl PiMigrationState {
    fn empty() -> Self {
        Self {
            version: MIGRATION_STATE_VERSION,
            ..Self::default()
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.version != MIGRATION_STATE_VERSION {
            anyhow::bail!("unsupported Pi migration state version")
        }
        for hash in self
            .skills
            .values()
            .chain(self.mcp_servers.values())
            .map(|entry| entry.hash.as_str())
        {
            if !is_sha256(hash) {
                anyhow::bail!("Pi migration state has an invalid content hash")
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum ChangePrivacy {
    Regular,
    Private,
}

#[derive(Clone, Debug)]
struct PlannedChange {
    target: PathBuf,
    relative_target: String,
    original: Option<Vec<u8>>,
    desired: Vec<u8>,
    limit: usize,
    privacy: ChangePrivacy,
}

#[derive(Clone, Debug)]
struct RestoreChange {
    target: PathBuf,
    relative_target: String,
    expected: Option<Vec<u8>>,
    desired: Option<Vec<u8>>,
    privacy: ChangePrivacy,
    limit: usize,
}

#[derive(Clone, Debug, Default)]
struct PlanCounts {
    models: usize,
    skills: usize,
    mcp_servers: usize,
    unchanged: usize,
    skipped: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ModelImportDiagnostic {
    path: String,
    reason: &'static str,
}

#[derive(Clone, Debug, Default)]
struct ModelSelection {
    model: Option<String>,
    diagnostics: Vec<ModelImportDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CatalogModelResolution {
    Resolved(String),
    Unknown,
    Ambiguous,
}

#[derive(Clone, Debug)]
struct Conflict {
    target: String,
    key: String,
}

#[derive(Clone, Debug)]
struct IngestionPlan {
    changes: Vec<PlannedChange>,
    conflicts: Vec<Conflict>,
    counts: PlanCounts,
    diagnostic_count: usize,
    model_diagnostics: Vec<ModelImportDiagnostic>,
}

#[derive(Clone, Debug)]
struct DesiredSkill {
    name: String,
    content: Vec<u8>,
    hash: String,
}

#[derive(Clone, Debug)]
struct DesiredMcpServer {
    name: String,
    value: Value,
    hash: String,
}

fn build_ingestion_plan(
    paths: &MigrationPaths,
    setup: &MigratedSetup,
    accept_conflicts: bool,
) -> anyhow::Result<IngestionPlan> {
    let mut counts = PlanCounts {
        skipped: setup.diagnostics().len(),
        ..PlanCounts::default()
    };
    let desired_skills = desired_skills(setup, &mut counts)?;
    let desired_mcp = desired_mcp_servers(setup, &mut counts)?;
    let model_selection = selected_model(setup, &mut counts)?;
    let diagnostic_count = setup.diagnostics().len() + model_selection.diagnostics.len();
    let selected_model = model_selection.model;
    let model_diagnostics = model_selection.diagnostics;
    let has_desired_items =
        selected_model.is_some() || !desired_skills.is_empty() || !desired_mcp.is_empty();
    if !has_desired_items {
        return Ok(IngestionPlan {
            changes: Vec::new(),
            conflicts: Vec::new(),
            counts,
            diagnostic_count,
            model_diagnostics,
        });
    }

    let (state_original, mut state) = load_state(paths)?;
    let mut conflicts = Vec::new();
    let mut changes = Vec::new();

    if let Some(model) = selected_model {
        let original = read_optional_regular(&paths.config, MAX_CONFIG_BYTES)?;
        let current = current_model(original.as_deref(), &paths.config)?;
        let prior = state.model.as_deref();
        let conflict = current.as_deref() != Some(model.as_str())
            && !(prior.is_none() && current.is_none())
            && prior != current.as_deref();
        if conflict {
            conflicts.push(Conflict {
                target: relative_home_path(&paths.home, &paths.config)?,
                key: "model".to_owned(),
            });
        }
        if !conflict || accept_conflicts {
            if current.as_deref() != Some(model.as_str()) {
                let original_text = original
                    .as_deref()
                    .map(|bytes| {
                        std::str::from_utf8(bytes)
                            .map_err(|_| anyhow::anyhow!("octet config is not valid UTF-8"))
                    })
                    .transpose()?;
                let desired = crate::cli::render_model_persistence_update(
                    original_text,
                    &paths.config,
                    &model,
                )?
                .into_bytes();
                changes.push(PlannedChange {
                    target: paths.config.clone(),
                    relative_target: relative_home_path(&paths.home, &paths.config)?,
                    original,
                    desired,
                    limit: MAX_CONFIG_BYTES,
                    privacy: ChangePrivacy::Regular,
                });
                counts.models += 1;
            } else {
                counts.unchanged += 1;
            }
            state.model = Some(model);
        }
    }

    for desired in desired_skills.values() {
        let target = paths.skills.join(&desired.name).join("SKILL.md");
        let original = read_optional_private(&target, MAX_SKILL_BYTES)?;
        let current_hash = original.as_deref().map(sha256_hex);
        let prior = state
            .skills
            .get(&desired.name)
            .map(|entry| entry.hash.as_str());
        let conflict = match current_hash.as_deref() {
            Some(hash) if hash == desired.hash => false,
            Some(hash) => prior != Some(hash),
            None => prior.is_some(),
        };
        if conflict {
            conflicts.push(Conflict {
                target: relative_home_path(&paths.home, &target)?,
                key: format!("skill {}", desired.name),
            });
        }
        if !conflict || accept_conflicts {
            if current_hash.as_deref() != Some(desired.hash.as_str()) {
                changes.push(PlannedChange {
                    target: target.clone(),
                    relative_target: relative_home_path(&paths.home, &target)?,
                    original,
                    desired: desired.content.clone(),
                    limit: MAX_SKILL_BYTES,
                    privacy: ChangePrivacy::Private,
                });
                counts.skills += 1;
            } else {
                counts.unchanged += 1;
            }
            state.skills.insert(
                desired.name.clone(),
                StateEntry {
                    hash: desired.hash.clone(),
                },
            );
        }
    }

    if !desired_mcp.is_empty() {
        let (mcp_original, mut mcp) = load_mcp_config(paths)?;
        let servers = mcp
            .get_mut("servers")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow::anyhow!("octet MCP config has no servers object"))?;
        let mut mcp_changed = false;
        for desired in desired_mcp.values() {
            let current = servers.get(&desired.name);
            let current_hash = current.map(canonical_hash).transpose()?;
            let prior = state
                .mcp_servers
                .get(&desired.name)
                .map(|entry| entry.hash.as_str());
            let conflict = match current_hash.as_deref() {
                Some(hash) if hash == desired.hash => false,
                Some(hash) => prior != Some(hash),
                None => prior.is_some(),
            };
            if conflict {
                conflicts.push(Conflict {
                    target: relative_home_path(&paths.home, &paths.mcp)?,
                    key: format!("MCP server {}", desired.name),
                });
            }
            if !conflict || accept_conflicts {
                if current_hash.as_deref() != Some(desired.hash.as_str()) {
                    servers.insert(desired.name.clone(), desired.value.clone());
                    mcp_changed = true;
                    counts.mcp_servers += 1;
                } else {
                    counts.unchanged += 1;
                }
                state.mcp_servers.insert(
                    desired.name.clone(),
                    StateEntry {
                        hash: desired.hash.clone(),
                    },
                );
            }
        }
        if mcp_changed {
            let desired = pretty_json_bytes(&mcp)?;
            if desired.len() > MAX_MCP_CONFIG_BYTES {
                anyhow::bail!("updated octet MCP config exceeds its size limit")
            }
            changes.push(PlannedChange {
                target: paths.mcp.clone(),
                relative_target: relative_home_path(&paths.home, &paths.mcp)?,
                original: mcp_original,
                desired,
                limit: MAX_MCP_CONFIG_BYTES,
                privacy: ChangePrivacy::Private,
            });
        }
    }

    state.validate()?;
    let state_desired = pretty_json_bytes(&state)?;
    if state_desired.len() > MAX_STATE_BYTES {
        anyhow::bail!("Pi migration state exceeds its size limit")
    }
    if has_desired_items && state_original.as_deref() != Some(state_desired.as_slice()) {
        changes.push(PlannedChange {
            target: paths.state.clone(),
            relative_target: relative_home_path(&paths.home, &paths.state)?,
            original: state_original,
            desired: state_desired,
            limit: MAX_STATE_BYTES,
            privacy: ChangePrivacy::Private,
        });
    }

    // Publish state last. A successful state record is therefore never left
    // behind when a prior destination write failed and was rolled back.
    changes.sort_by(|left, right| {
        let left_state = left.target == paths.state;
        let right_state = right.target == paths.state;
        left_state
            .cmp(&right_state)
            .then_with(|| left.target.cmp(&right.target))
    });
    Ok(IngestionPlan {
        changes,
        conflicts,
        counts,
        diagnostic_count,
        model_diagnostics,
    })
}

fn desired_skills(
    setup: &MigratedSetup,
    counts: &mut PlanCounts,
) -> anyhow::Result<BTreeMap<String, DesiredSkill>> {
    let mut desired = BTreeMap::new();
    for outcome in setup.skills() {
        let Some((_path, skill)) = outcome.as_mapped() else {
            counts.skipped += 1;
            continue;
        };
        if !valid_skill_name(skill.name()) {
            counts.skipped += 1;
            continue;
        }
        let content = disabled_skill_content(skill.name(), skill.content()).into_bytes();
        if content.len() > MAX_SKILL_BYTES {
            counts.skipped += 1;
            continue;
        }
        desired.insert(
            skill.name().to_owned(),
            DesiredSkill {
                name: skill.name().to_owned(),
                hash: sha256_hex(&content),
                content,
            },
        );
    }
    Ok(desired)
}

fn disabled_skill_content(name: &str, source: &str) -> String {
    // Do not trust or execute source frontmatter. The original text remains
    // intact below a host-authored, disabled review envelope.
    format!(
        "---\nname: {name}\ndescription: Imported Pi skill; review before enabling.\ndisable-model-invocation: true\nmetadata:\n  migration:\n    source: pi\n    review_required: true\n---\n\n{source}"
    )
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn desired_mcp_servers(
    setup: &MigratedSetup,
    counts: &mut PlanCounts,
) -> anyhow::Result<BTreeMap<String, DesiredMcpServer>> {
    let mut desired = BTreeMap::new();
    for outcome in setup.mcp_servers() {
        let Some((_path, server)) = outcome.as_mapped() else {
            counts.skipped += 1;
            continue;
        };
        if !valid_mcp_server_name(server.name()) {
            counts.skipped += 1;
            continue;
        }
        let Some(command) = server.transport().command() else {
            counts.skipped += 1;
            continue;
        };
        let args = server.transport().args().unwrap_or_default();
        if command.is_empty()
            || command.chars().any(char::is_control)
            || args.len() > 64
            || args.iter().any(|arg| arg.chars().any(char::is_control))
        {
            counts.skipped += 1;
            continue;
        }
        let value = json!({
            "transport":"stdio",
            "label":format!("Imported Pi: {}", server.name()),
            "command":command,
            "args":args,
            "enabled":false,
            "required":false,
        });
        let hash = canonical_hash(&value)?;
        desired.insert(
            server.name().to_owned(),
            DesiredMcpServer {
                name: server.name().to_owned(),
                value,
                hash,
            },
        );
    }
    Ok(desired)
}

fn valid_mcp_server_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn selected_model(
    setup: &MigratedSetup,
    counts: &mut PlanCounts,
) -> anyhow::Result<ModelSelection> {
    let catalog = ModelCatalog::builtin()
        .map_err(|error| anyhow::anyhow!("cannot load octet's static model catalog: {error}"))?;
    Ok(selected_model_in_catalog(setup, &catalog, counts))
}

fn selected_model_in_catalog(
    setup: &MigratedSetup,
    catalog: &ModelCatalog,
    counts: &mut PlanCounts,
) -> ModelSelection {
    let mut selection = ModelSelection::default();
    for outcome in setup.models() {
        // Model outcomes are ordered by source precedence. An unresolved later
        // selection must not leave an earlier model configured by accident.
        selection.model = None;
        let Some((path, model)) = outcome.as_mapped() else {
            counts.skipped += 1;
            continue;
        };
        match resolve_catalog_model(catalog, model.provider(), model.model()) {
            CatalogModelResolution::Resolved(model) => selection.model = Some(model),
            CatalogModelResolution::Unknown => {
                counts.skipped += 1;
                selection.diagnostics.push(ModelImportDiagnostic {
                    path: path.to_owned(),
                    reason: UNKNOWN_MODEL_DIAGNOSTIC,
                });
            }
            CatalogModelResolution::Ambiguous => {
                counts.skipped += 1;
                selection.diagnostics.push(ModelImportDiagnostic {
                    path: path.to_owned(),
                    reason: AMBIGUOUS_MODEL_DIAGNOSTIC,
                });
            }
        }
    }
    selection
}

/// Pi identifies models by provider API name. Select a octet config ID only when
/// that pair has exactly one matching built-in endpoint and API name.
fn resolve_catalog_model(
    catalog: &ModelCatalog,
    provider: &str,
    api_name: &str,
) -> CatalogModelResolution {
    let provider = canonical_model_provider(provider);
    let mut candidates = catalog
        .models()
        .filter(|candidate| candidate.endpoint.0 == provider && candidate.api_name == api_name);
    let Some(candidate) = candidates.next() else {
        return CatalogModelResolution::Unknown;
    };
    if candidates.next().is_some() {
        return CatalogModelResolution::Ambiguous;
    }
    CatalogModelResolution::Resolved(candidate.id.0.clone())
}

fn canonical_model_provider(provider: &str) -> &str {
    match provider {
        "google-ai" | "google-generative-ai" => "google",
        "openai-codex" => "codex",
        provider => provider,
    }
}

fn load_state(paths: &MigrationPaths) -> anyhow::Result<(Option<Vec<u8>>, PiMigrationState)> {
    let original = read_optional_private(&paths.state, MAX_STATE_BYTES)?;
    let state = match original.as_deref() {
        None => PiMigrationState::empty(),
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| {
            anyhow::anyhow!("Pi migration state is invalid; refuse to overwrite it")
        })?,
    };
    state.validate()?;
    Ok((original, state))
}

fn load_mcp_config(paths: &MigrationPaths) -> anyhow::Result<(Option<Vec<u8>>, Value)> {
    let original = read_optional_private(&paths.mcp, MAX_MCP_CONFIG_BYTES)?;
    let mut value = match original.as_deref() {
        None => json!({"version":1,"servers":{}}),
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| {
            anyhow::anyhow!("octet MCP config is invalid JSON; refuse to overwrite it")
        })?,
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("octet MCP config root must be an object"))?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        anyhow::bail!("octet MCP config must have version 1")
    }
    if !object.get("servers").is_some_and(Value::is_object) {
        anyhow::bail!("octet MCP config must have a servers object")
    }
    Ok((original, value))
}

fn current_model(original: Option<&[u8]>, path: &Path) -> anyhow::Result<Option<String>> {
    let Some(bytes) = original else {
        return Ok(None);
    };
    let source = std::str::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("octet config {} is not valid UTF-8", path.display()))?;
    if source.trim().is_empty() {
        return Ok(None);
    }
    let document = source.parse::<toml_edit::DocumentMut>().map_err(|error| {
        anyhow::anyhow!("cannot update invalid config {}: {error}", path.display())
    })?;
    let Some(item) = document.get("model") else {
        return Ok(None);
    };
    item.as_str()
        .map(str::to_owned)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("octet config model must be a string"))
}

fn read_optional_regular(path: &Path, limit: usize) -> anyhow::Result<Option<Vec<u8>>> {
    match octet_agent::secure_fs::read_regular_file_bounded(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(anyhow::anyhow!(
            "cannot safely read {}: {error}",
            path.display()
        )),
    }
}

fn read_optional_private(path: &Path, limit: usize) -> anyhow::Result<Option<Vec<u8>>> {
    match octet_agent::secure_fs::read_private_file_bounded(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(anyhow::anyhow!(
            "cannot safely read private migration target {}: {error}",
            path.display()
        )),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BackupManifest {
    version: u32,
    created_at_unix_ms: u128,
    source: String,
    entries: Vec<BackupEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BackupEntry {
    target: String,
    backup: Option<String>,
    before_sha256: Option<String>,
    after_sha256: String,
    private: bool,
}

fn apply_ingestion_plan(
    paths: &MigrationPaths,
    plan: &IngestionPlan,
) -> anyhow::Result<Option<PathBuf>> {
    if plan.changes.is_empty() {
        return Ok(None);
    }
    let backup = create_backup(paths, &plan.changes)?;
    let mut committed = Vec::new();
    for change in &plan.changes {
        if let Err(error) = write_planned_change(change) {
            let rollback_errors = rollback_changes(&committed);
            let recovery = format!(
                " Backup retained at {}. Restore with `octet migrate restore {}`.",
                backup.display(),
                backup.display()
            );
            if rollback_errors.is_empty() {
                anyhow::bail!(
                    "migration failed while updating {} and was rolled back: {error}.{recovery}",
                    change.relative_target
                )
            }
            anyhow::bail!(
                "migration failed while updating {}; automatic rollback was incomplete ({} target(s)).{recovery}",
                change.relative_target,
                rollback_errors.len()
            )
        }
        committed.push(change.clone());
    }
    Ok(Some(backup))
}

fn write_planned_change(change: &PlannedChange) -> anyhow::Result<()> {
    let result = match change.privacy {
        ChangePrivacy::Regular => octet_agent::secure_fs::write_atomic_if_unchanged(
            &change.target,
            change.original.as_deref(),
            &change.desired,
            change.limit,
        ),
        ChangePrivacy::Private => octet_agent::secure_fs::write_private_atomic_if_unchanged(
            &change.target,
            change.original.as_deref(),
            &change.desired,
            change.limit,
        ),
    };
    result.map_err(|error| {
        anyhow::anyhow!(
            "atomic compare-and-swap refused {}: {error}",
            change.relative_target
        )
    })
}

fn rollback_changes(changes: &[PlannedChange]) -> Vec<anyhow::Error> {
    let mut errors = Vec::new();
    for change in changes.iter().rev() {
        let result = match &change.original {
            Some(original) => match change.privacy {
                ChangePrivacy::Regular => octet_agent::secure_fs::write_atomic_if_unchanged(
                    &change.target,
                    Some(&change.desired),
                    original,
                    change.limit,
                ),
                ChangePrivacy::Private => {
                    octet_agent::secure_fs::write_private_atomic_if_unchanged(
                        &change.target,
                        Some(&change.desired),
                        original,
                        change.limit,
                    )
                }
            },
            None => remove_created_change(change),
        };
        if let Err(error) = result {
            errors.push(anyhow::anyhow!(
                "could not roll back {}: {error}",
                change.relative_target
            ));
        }
    }
    errors
}

fn remove_created_change(
    change: &PlannedChange,
) -> Result<(), octet_agent::secure_fs::SecureFileError> {
    match change.privacy {
        ChangePrivacy::Regular => octet_agent::secure_fs::remove_regular_file_if_unchanged(
            &change.target,
            &change.desired,
            change.limit,
        ),
        ChangePrivacy::Private => octet_agent::secure_fs::remove_private_file_if_unchanged(
            &change.target,
            &change.desired,
            change.limit,
        ),
    }
}

fn create_backup(paths: &MigrationPaths, changes: &[PlannedChange]) -> anyhow::Result<PathBuf> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow::anyhow!("system clock is before the Unix epoch"))?
        .as_millis();
    let backup = octet_agent::secure_fs::create_unique_private_directory(
        &paths.backups,
        &format!("{millis}-"),
    )
    .map_err(|error| anyhow::anyhow!("cannot create private migration backup: {error}"))?;
    let mut entries = Vec::with_capacity(changes.len());
    for (index, change) in changes.iter().enumerate() {
        let backup_name = change.original.as_ref().map(|_| format!("{index:03}.bin"));
        if let (Some(name), Some(original)) = (&backup_name, &change.original) {
            octet_agent::secure_fs::write_private_atomic(
                &backup.join(name),
                original,
                change.limit,
            )
            .map_err(|error| anyhow::anyhow!("cannot write private migration backup: {error}"))?;
        }
        entries.push(BackupEntry {
            target: change.relative_target.clone(),
            backup: backup_name,
            before_sha256: change.original.as_deref().map(sha256_hex),
            after_sha256: sha256_hex(&change.desired),
            private: matches!(change.privacy, ChangePrivacy::Private),
        });
    }
    let manifest = BackupManifest {
        version: BACKUP_VERSION,
        created_at_unix_ms: millis,
        source: "pi".to_owned(),
        entries,
    };
    let bytes = pretty_json_bytes(&manifest)?;
    octet_agent::secure_fs::write_private_atomic(
        &backup.join("manifest.json"),
        &bytes,
        MAX_BACKUP_MANIFEST_BYTES,
    )
    .map_err(|error| anyhow::anyhow!("cannot write migration backup manifest: {error}"))?;
    Ok(backup)
}

fn restore_backup(paths: &MigrationPaths, backup: &Path, force: bool) -> anyhow::Result<usize> {
    let backup = authorized_backup_path(paths, backup)?;
    let manifest_path = backup.join("manifest.json");
    let bytes = read_optional_private(&manifest_path, MAX_BACKUP_MANIFEST_BYTES)?
        .ok_or_else(|| anyhow::anyhow!("migration backup manifest is missing"))?;
    let manifest: BackupManifest = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("migration backup manifest is invalid"))?;
    validate_backup_manifest(&manifest)?;

    let restored = manifest.entries.len();
    let mut changes = Vec::with_capacity(restored);
    for entry in manifest.entries {
        let (target, privacy) = target_from_backup_entry(paths, &entry.target)?;
        if entry.private != matches!(privacy, ChangePrivacy::Private) {
            anyhow::bail!("migration backup entry has an invalid privacy class")
        }
        let current = match privacy {
            ChangePrivacy::Private => read_optional_private(&target, MAX_CONFIG_BYTES)?,
            ChangePrivacy::Regular => read_optional_regular(&target, MAX_CONFIG_BYTES)?,
        };
        let current_hash = current.as_deref().map(sha256_hex);
        if !force && current_hash.as_deref() != Some(entry.after_sha256.as_str()) {
            anyhow::bail!(
                "{} changed after import; review it and rerun restore with --yes to overwrite it",
                entry.target
            )
        }
        let desired = match entry.backup {
            Some(name) => {
                let original = read_optional_private(&backup.join(name), MAX_CONFIG_BYTES)?
                    .ok_or_else(|| anyhow::anyhow!("migration backup payload is missing"))?;
                let before = entry
                    .before_sha256
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("migration backup payload hash is missing"))?;
                if sha256_hex(&original) != before {
                    anyhow::bail!("migration backup payload does not match its manifest hash")
                }
                Some(original)
            }
            None => None,
        };
        if current.is_some() || desired.is_some() {
            changes.push(RestoreChange {
                target,
                relative_target: entry.target,
                expected: current,
                desired,
                privacy,
                limit: MAX_CONFIG_BYTES,
            });
        }
    }
    apply_restore_changes(&changes)?;
    Ok(restored)
}

fn apply_restore_changes(changes: &[RestoreChange]) -> anyhow::Result<()> {
    let mut committed = Vec::new();
    for change in changes {
        if let Err(error) = write_restore_change(change) {
            let rollback_errors = rollback_restore_changes(&committed);
            if rollback_errors.is_empty() {
                anyhow::bail!(
                    "restore failed while updating {} and was rolled back: {error}",
                    change.relative_target
                )
            }
            anyhow::bail!(
                "restore failed while updating {}; automatic rollback was incomplete ({} target(s))",
                change.relative_target,
                rollback_errors.len()
            )
        }
        committed.push(change.clone());
    }
    Ok(())
}

fn write_restore_change(
    change: &RestoreChange,
) -> Result<(), octet_agent::secure_fs::SecureFileError> {
    match (&change.expected, &change.desired) {
        (_, Some(desired)) => write_change_bytes(
            change.privacy,
            &change.target,
            change.expected.as_deref(),
            desired,
            change.limit,
        ),
        (Some(expected), None) => {
            remove_change_bytes(change.privacy, &change.target, expected, change.limit)
        }
        (None, None) => Ok(()),
    }
}

fn rollback_restore_changes(changes: &[RestoreChange]) -> Vec<anyhow::Error> {
    let mut errors = Vec::new();
    for change in changes.iter().rev() {
        let result = match (&change.expected, &change.desired) {
            (Some(original), Some(restored)) => write_change_bytes(
                change.privacy,
                &change.target,
                Some(restored),
                original,
                change.limit,
            ),
            (Some(original), None) => {
                write_change_bytes(change.privacy, &change.target, None, original, change.limit)
            }
            (None, Some(restored)) => {
                remove_change_bytes(change.privacy, &change.target, restored, change.limit)
            }
            (None, None) => Ok(()),
        };
        if let Err(error) = result {
            errors.push(anyhow::anyhow!(
                "could not roll back restored {}: {error}",
                change.relative_target
            ));
        }
    }
    errors
}

fn write_change_bytes(
    privacy: ChangePrivacy,
    target: &Path,
    expected: Option<&[u8]>,
    desired: &[u8],
    limit: usize,
) -> Result<(), octet_agent::secure_fs::SecureFileError> {
    match privacy {
        ChangePrivacy::Regular => {
            octet_agent::secure_fs::write_atomic_if_unchanged(target, expected, desired, limit)
        }
        ChangePrivacy::Private => octet_agent::secure_fs::write_private_atomic_if_unchanged(
            target, expected, desired, limit,
        ),
    }
}

fn remove_change_bytes(
    privacy: ChangePrivacy,
    target: &Path,
    expected: &[u8],
    limit: usize,
) -> Result<(), octet_agent::secure_fs::SecureFileError> {
    match privacy {
        ChangePrivacy::Regular => {
            octet_agent::secure_fs::remove_regular_file_if_unchanged(target, expected, limit)
        }
        ChangePrivacy::Private => {
            octet_agent::secure_fs::remove_private_file_if_unchanged(target, expected, limit)
        }
    }
}

fn authorized_backup_path(paths: &MigrationPaths, backup: &Path) -> anyhow::Result<PathBuf> {
    let root = paths
        .backups
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("migration backup root does not exist"))?;
    let backup = backup
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("migration backup path does not exist"))?;
    if backup.parent() != Some(root.as_path()) {
        anyhow::bail!("backup path is outside this octet home's migration backup directory")
    }
    Ok(backup)
}

fn validate_backup_manifest(manifest: &BackupManifest) -> anyhow::Result<()> {
    if manifest.version != BACKUP_VERSION || manifest.source != "pi" || manifest.entries.is_empty()
    {
        anyhow::bail!("unsupported migration backup manifest")
    }
    if manifest.entries.len() > MAX_SOURCE_ITEMS.saturating_add(4) {
        anyhow::bail!("migration backup manifest has too many entries")
    }
    let mut targets = BTreeSet::new();
    let mut payloads = BTreeSet::new();
    for entry in &manifest.entries {
        if !targets.insert(&entry.target) {
            anyhow::bail!("migration backup manifest has duplicate targets")
        }
        validate_relative_target(&entry.target)?;
        if !is_sha256(&entry.after_sha256)
            || entry
                .before_sha256
                .as_deref()
                .is_some_and(|hash| !is_sha256(hash))
        {
            anyhow::bail!("migration backup manifest has an invalid hash")
        }
        if entry.backup.is_some() != entry.before_sha256.is_some() {
            anyhow::bail!("migration backup manifest has inconsistent payload metadata")
        }
        if let Some(name) = &entry.backup {
            if !payloads.insert(name) {
                anyhow::bail!("migration backup manifest has duplicate payload names")
            }
            if name.contains('/')
                || name.contains('\\')
                || !name.ends_with(".bin")
                || name.len() > 32
            {
                anyhow::bail!("migration backup manifest has an invalid payload name")
            }
        }
    }
    Ok(())
}

fn target_from_backup_entry(
    paths: &MigrationPaths,
    relative: &str,
) -> anyhow::Result<(PathBuf, ChangePrivacy)> {
    validate_relative_target(relative)?;
    let target = paths.home.join(relative);
    if !target.starts_with(&paths.home) {
        anyhow::bail!("migration backup target escaped the octet home")
    }
    if target == paths.config {
        return Ok((target, ChangePrivacy::Regular));
    }
    if target == paths.mcp || target == paths.state {
        return Ok((target, ChangePrivacy::Private));
    }
    let skill = target
        .strip_prefix(&paths.skills)
        .ok()
        .and_then(|relative| {
            let mut components = relative.components();
            match (components.next(), components.next(), components.next()) {
                (Some(Component::Normal(name)), Some(Component::Normal(file)), None)
                    if file == "SKILL.md" =>
                {
                    name.to_str().filter(|name| valid_skill_name(name))
                }
                _ => None,
            }
        });
    if skill.is_some() {
        return Ok((target, ChangePrivacy::Private));
    }
    anyhow::bail!("migration backup target is not an import-managed destination")
}

fn validate_relative_target(path: &str) -> anyhow::Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        anyhow::bail!("migration backup target is invalid")
    }
    let value = Path::new(path);
    if value.is_absolute()
        || value
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("migration backup target is invalid")
    }
    Ok(())
}

fn relative_home_path(home: &Path, target: &Path) -> anyhow::Result<String> {
    let relative = target
        .strip_prefix(home)
        .map_err(|_| anyhow::anyhow!("migration target escaped home"))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            anyhow::bail!("migration target is not normalized")
        };
        parts.push(
            part.to_str()
                .ok_or_else(|| anyhow::anyhow!("migration target is not valid UTF-8"))?,
        );
    }
    if parts.is_empty() {
        anyhow::bail!("migration target must not be the home directory")
    }
    Ok(parts.join("/"))
}

fn pretty_json_bytes(value: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_hash(value: &Value) -> anyhow::Result<String> {
    let encoded = api::canonical_json(value)
        .map_err(|error| anyhow::anyhow!("cannot canonicalize MCP server: {error}"))?;
    Ok(sha256_hex(encoded.as_bytes()))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn confirm_conflicts(conflicts: &[Conflict], yes: bool) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "migration found {} conflicting current entries; rerun with --yes after review",
            conflicts.len()
        )
    }
    crate::output::stdout_line(format!(
        "Migration found {} conflicting current entry(s):",
        conflicts.len()
    ));
    for conflict in conflicts {
        crate::output::stdout_line(format!("  {} ({})", conflict.target, conflict.key));
    }
    crate::output::stdout_line("Overwrite these entries? [y/N]");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

#[derive(Serialize)]
struct PublicImportReport<'a> {
    source: String,
    dry_run: bool,
    models_updated: usize,
    skills_disabled: usize,
    mcp_servers_disabled: usize,
    unchanged: usize,
    skipped: usize,
    diagnostics: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    model_diagnostics: Vec<ModelImportDiagnostic>,
    conflicts: usize,
    backup: Option<&'a Path>,
}

fn emit_no_source_report(source: &Path, json_output: bool) {
    if json_output {
        crate::output::stdout_multiline(
            serde_json::to_string_pretty(&json!({
                "source":source,
                "detected":false,
                "changed":false,
            }))
            .expect("static no-source report serializes"),
        );
    } else {
        crate::output::stdout_line(format!(
            "No Pi setup was detected at {}; no files were changed.",
            source.display()
        ));
    }
}

fn emit_import_report(
    source: &Path,
    plan: &IngestionPlan,
    backup: Option<&Path>,
    dry_run: bool,
    json_output: bool,
) {
    let report = PublicImportReport {
        source: source.display().to_string(),
        dry_run,
        models_updated: plan.counts.models,
        skills_disabled: plan.counts.skills,
        mcp_servers_disabled: plan.counts.mcp_servers,
        unchanged: plan.counts.unchanged,
        skipped: plan.counts.skipped,
        diagnostics: plan.diagnostic_count,
        model_diagnostics: plan.model_diagnostics.clone(),
        conflicts: plan.conflicts.len(),
        backup,
    };
    if json_output {
        crate::output::stdout_multiline(
            serde_json::to_string_pretty(&report).expect("public migration report serializes"),
        );
        return;
    }
    let action = if dry_run {
        "Pi migration import preview"
    } else {
        "Pi migration import complete"
    };
    crate::output::stdout_line(action);
    crate::output::stdout_line(format!("  Source: {}", source.display()));
    crate::output::stdout_line(format!("  Model updates: {}", report.models_updated));
    crate::output::stdout_line(format!(
        "  Disabled skills awaiting review: {}",
        report.skills_disabled
    ));
    crate::output::stdout_line(format!(
        "  Disabled MCP servers awaiting review: {}",
        report.mcp_servers_disabled
    ));
    crate::output::stdout_line(format!("  Unchanged: {}", report.unchanged));
    crate::output::stdout_line(format!("  Skipped: {}", report.skipped));
    if !report.model_diagnostics.is_empty() {
        crate::output::stdout_line("  Model diagnostics:");
        for diagnostic in &report.model_diagnostics {
            crate::output::stdout_line(format!("    {}: {}", diagnostic.path, diagnostic.reason));
        }
    }
    if report.conflicts > 0 {
        crate::output::stdout_line(format!("  Conflicts: {}", report.conflicts));
    }
    if let Some(backup) = backup {
        crate::output::stdout_line(format!("  Backup: {}", backup.display()));
    }
    crate::output::stdout_line(
        "  Credentials, MCP environment values, headers, and Pi permissions were not copied.",
    );
    if report.skills_disabled > 0 || report.mcp_servers_disabled > 0 {
        crate::output::stdout_line(
            "  Review imported entries before enabling skills or MCP servers; migration never enables extensions.",
        );
    }
}

#[cfg(test)]
mod tests;

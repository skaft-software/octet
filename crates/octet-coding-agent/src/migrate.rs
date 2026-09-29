//! The legacy `pi` migration scanner: it reads a foreign `pi` installation and
//! reports - without executing any of it - what can be migrated automatically.
//!
//! This module owns the report shapes, the settings and package resolution, the
//! per-package scan and the human-readable rendering. The two halves of the scan
//! that actually walk someone else's disk live beside it:
//! [`resource_collection`](self::resource_collection) enumerates the resources a
//! package ships, and [`source_analysis`](self::source_analysis) reads an
//! extension's source to decide how far it can be trusted.
#![allow(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use clap::Subcommand;
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod migration_import;
mod resource_collection;
mod source_analysis;

#[cfg(test)]
use resource_collection::collect_resource_path;
use resource_collection::{
    collect_package_resources, collect_top_level_resources, is_package_source_file,
    walk_regular_files,
};
#[cfg(test)]
use source_analysis::analyze_extension;
use source_analysis::analyze_extension_with_budget;

const MAX_SETTINGS_BYTES: usize = 1024 * 1024;
const MAX_PACKAGES: usize = 256;
const MAX_PATTERNS: usize = 1024;
const MAX_PACKAGE_JSON_BYTES: usize = 1024 * 1024;
const MAX_SOURCE_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOCK_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESOURCE_FILES: usize = 4096;
const MAX_WALK_ENTRIES: usize = 16_384;
const MAX_ANALYZED_FILES: usize = 512;
const MAX_EXTENSION_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_AST_NODES: usize = 2_000_000;
const MAX_SCAN_SOURCE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SCAN_AST_NODES: usize = 8_000_000;
const MAX_SCAN_ANALYZED_FILES: usize = 8192;
const MAX_SCAN_RESOURCES: usize = 32_768;
const MAX_SCAN_HASH_BYTES: usize = 256 * 1024 * 1024;
const MAX_PACKAGE_DIAGNOSTICS: usize = 256;
const MAX_REPORT_DIAGNOSTICS: usize = 1024;
const MAX_PACKAGE_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EXTENSION_MANIFEST_DEPTH: usize = 64;

#[derive(Clone, Debug, Subcommand)]
pub enum MigrationCommand {
    /// Inventory a Pi setup without executing packages or invoking a model.
    Pi {
        /// Explicitly state that this invocation must not modify either setup.
        #[arg(long)]
        dry_run: bool,
        /// Emit the versioned machine-readable report.
        #[arg(long, conflicts_with = "summary")]
        json: bool,
        /// Emit only aggregate counts and diagnostics.
        #[arg(long)]
        summary: bool,
        /// Pi's user agent directory (defaults to PI_CODING_AGENT_DIR or ~/.pi/agent).
        #[arg(long, value_name = "DIR")]
        pi_home: Option<PathBuf>,
        /// Project whose .pi/settings.json and resources should be inspected.
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        /// Additional legacy global npm node_modules root (repeatable).
        #[arg(long = "npm-root", value_name = "DIR")]
        npm_roots: Vec<PathBuf>,
    },
    /// Apply a bounded, host-owned import from a supported source setup.
    Import {
        #[command(subcommand)]
        command: MigrationImportCommand,
    },
    /// Restore one retained migration backup after verifying destination state.
    Restore {
        /// Backup directory printed by a migration import.
        #[arg(value_name = "BACKUP")]
        backup: PathBuf,
        /// Overwrite targets changed after the import.
        #[arg(long)]
        yes: bool,
    },
    /// Internal API 0.3 migration adapter process entrypoint.
    #[command(hide = true)]
    Adapter {
        #[command(subcommand)]
        command: MigrationAdapterCommand,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum MigrationImportCommand {
    /// Import a Pi setup through the read-only typed adapter.
    Pi {
        /// Explicit Pi source directory. Without it, standard macOS/Linux locations are checked.
        #[arg(long, value_name = "DIR")]
        source: Option<PathBuf>,
        /// Accept current-entry conflicts without an interactive prompt.
        #[arg(long)]
        yes: bool,
        /// Validate, normalize, and report changes without writing destination files.
        #[arg(long)]
        dry_run: bool,
        /// Emit a non-secret machine-readable import summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum MigrationAdapterCommand {
    /// Pi's bounded read-only migration adapter.
    Pi,
}

#[derive(Clone, Debug)]
struct ScanOptions {
    pi_home: PathBuf,
    project: PathBuf,
    npm_roots: Vec<PathBuf>,
}

#[derive(Default)]
struct AnalysisBudget {
    source_bytes: usize,
    syntax_nodes: usize,
    files: usize,
    resources: usize,
    hashed_bytes: usize,
}

#[derive(Default)]
struct ResourceTraversal {
    active_extension_manifests: Vec<PathBuf>,
    extension_manifest_resolution_stopped: bool,
}

impl ResourceTraversal {
    fn extension_manifest_is_active(&self, path: &Path) -> bool {
        self.active_extension_manifests
            .iter()
            .any(|active| active == path)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    User,
    Project,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResourceKind {
    Extension,
    Skill,
    Prompt,
    Theme,
}

impl ResourceKind {
    const ALL: [Self; 4] = [Self::Extension, Self::Skill, Self::Prompt, Self::Theme];

    fn key(self) -> &'static str {
        match self {
            Self::Extension => "extensions",
            Self::Skill => "skills",
            Self::Prompt => "prompts",
            Self::Theme => "themes",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum MigrationPath {
    Direct,
    Replace,
    Bridge,
    NativePort,
    Manual,
    Blocked,
}

impl MigrationPath {
    fn label(self) -> &'static str {
        match self {
            Self::Direct => "DIRECT",
            Self::Replace => "REPLACE",
            Self::Bridge => "BRIDGE",
            Self::NativePort => "NATIVE PORT",
            Self::Manual => "MANUAL",
            Self::Blocked => "BLOCKED",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticLevel {
    Warning,
    Error,
}

#[derive(Clone, Debug, Serialize)]
struct Diagnostic {
    level: DiagnosticLevel,
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
}

impl Diagnostic {
    fn warning(code: &'static str, message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self {
            level: DiagnosticLevel::Warning,
            code,
            message: message.into(),
            path,
        }
    }

    fn error(code: &'static str, message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self {
            level: DiagnosticLevel::Error,
            code,
            message: message.into(),
            path,
        }
    }
}

fn cap_diagnostics(diagnostics: &mut Vec<Diagnostic>, limit: usize, path: Option<PathBuf>) {
    if diagnostics.len() <= limit {
        return;
    }
    let omitted = diagnostics.len() - limit + 1;
    diagnostics.truncate(limit.saturating_sub(1));
    diagnostics.push(Diagnostic::warning(
        "diagnostic_limit",
        format!("omitted {omitted} additional migration diagnostics"),
        path,
    ));
}

#[derive(Clone, Debug, Serialize)]
struct SettingsReport {
    scope: Scope,
    path: PathBuf,
    found: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct PiSettings {
    #[serde(default)]
    packages: Vec<PackageSetting>,
    extensions: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    prompts: Option<Vec<String>>,
    themes: Option<Vec<String>>,
}

impl PiSettings {
    fn overrides(&self, kind: ResourceKind) -> &[String] {
        match kind {
            ResourceKind::Extension => self.extensions.as_deref().unwrap_or(&[]),
            ResourceKind::Skill => self.skills.as_deref().unwrap_or(&[]),
            ResourceKind::Prompt => self.prompts.as_deref().unwrap_or(&[]),
            ResourceKind::Theme => self.themes.as_deref().unwrap_or(&[]),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum PackageSetting {
    Source(String),
    Filter(PackageFilter),
}

impl PackageSetting {
    fn source(&self) -> &str {
        match self {
            Self::Source(source) => source,
            Self::Filter(filter) => &filter.source,
        }
    }

    fn filter(&self) -> Option<&PackageFilter> {
        match self {
            Self::Source(_) => None,
            Self::Filter(filter) => Some(filter),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct PackageFilter {
    source: String,
    #[serde(default)]
    autoload: Option<bool>,
    extensions: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    prompts: Option<Vec<String>>,
    themes: Option<Vec<String>>,
}

impl PackageFilter {
    fn patterns(&self, kind: ResourceKind) -> Option<&[String]> {
        match kind {
            ResourceKind::Extension => self.extensions.as_deref(),
            ResourceKind::Skill => self.skills.as_deref(),
            ResourceKind::Prompt => self.prompts.as_deref(),
            ResourceKind::Theme => self.themes.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    pi: Option<PiManifest>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct PiManifest {
    extensions: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    prompts: Option<Vec<String>>,
    themes: Option<Vec<String>>,
}

impl PiManifest {
    fn entries(&self, kind: ResourceKind) -> Option<&[String]> {
        match kind {
            ResourceKind::Extension => self.extensions.as_deref(),
            ResourceKind::Skill => self.skills.as_deref(),
            ResourceKind::Prompt => self.prompts.as_deref(),
            ResourceKind::Theme => self.themes.as_deref(),
        }
    }
}

#[derive(Clone, Debug)]
struct ResolvedPackageSetting {
    identity: String,
    source: String,
    scope: Scope,
    root: Option<PathBuf>,
    single_extension: Option<PathBuf>,
    filter: Option<PackageFilter>,
    resolution_diagnostic: Option<Diagnostic>,
}

#[derive(Clone, Debug, Serialize)]
struct PackageReport {
    identity: String,
    source: String,
    scope: Scope,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_hash: Option<String>,
    migration: MigrationPath,
    resources: Vec<ResourceReport>,
    extensions: Vec<ExtensionReport>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Serialize)]
struct ResourceReport {
    kind: ResourceKind,
    scope: Scope,
    path: PathBuf,
    enabled: bool,
    migration: MigrationPath,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ExtensionSurfaces {
    events: Vec<String>,
    registrations: Vec<String>,
    actions: Vec<String>,
    ui: Vec<String>,
    mutations: Vec<String>,
    imports: Vec<String>,
    unresolved_imports: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct SecuritySignals {
    filesystem: bool,
    process: bool,
    network: bool,
    secrets: bool,
    native_modules: bool,
    dynamic_imports: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ExtensionReport {
    path: PathBuf,
    migration: MigrationPath,
    reasons: Vec<String>,
    analyzed_files: Vec<PathBuf>,
    analyzed_source_bytes: usize,
    syntax_nodes: usize,
    surfaces: ExtensionSurfaces,
    security: SecuritySignals,
    parse_errors: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ResourceCounts {
    packages: usize,
    extensions: usize,
    skills: usize,
    prompts: usize,
    themes: usize,
    disabled: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
struct MigrationCounts {
    direct: usize,
    replace: usize,
    bridge: usize,
    native_port: usize,
    manual: usize,
    blocked: usize,
}

impl MigrationCounts {
    fn add(&mut self, migration: MigrationPath) {
        match migration {
            MigrationPath::Direct => self.direct += 1,
            MigrationPath::Replace => self.replace += 1,
            MigrationPath::Bridge => self.bridge += 1,
            MigrationPath::NativePort => self.native_port += 1,
            MigrationPath::Manual => self.manual += 1,
            MigrationPath::Blocked => self.blocked += 1,
        }
    }

    fn get(&self, migration: MigrationPath) -> usize {
        match migration {
            MigrationPath::Direct => self.direct,
            MigrationPath::Replace => self.replace,
            MigrationPath::Bridge => self.bridge,
            MigrationPath::NativePort => self.native_port,
            MigrationPath::Manual => self.manual,
            MigrationPath::Blocked => self.blocked,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct MigrationReport {
    schema_version: u32,
    source: &'static str,
    mode: &'static str,
    model_usage: &'static str,
    package_code_executed: bool,
    settings: Vec<SettingsReport>,
    found: ResourceCounts,
    migration: MigrationCounts,
    packages: Vec<PackageReport>,
    extensions: Vec<ExtensionReport>,
    resources: Vec<ResourceReport>,
    diagnostics: Vec<Diagnostic>,
}

pub fn run(command: MigrationCommand, invocation_cwd: &Path) -> anyhow::Result<()> {
    match command {
        MigrationCommand::Pi {
            dry_run: _,
            json,
            summary,
            pi_home,
            project,
            npm_roots,
        } => {
            let project =
                absolute_path(project.as_deref().unwrap_or(invocation_cwd), invocation_cwd)?;
            let pi_home = match pi_home {
                Some(path) => absolute_path(&path, invocation_cwd)?,
                None => default_pi_home()?,
            };
            let npm_roots = npm_roots
                .iter()
                .map(|path| absolute_path(path, invocation_cwd))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let report = scan_pi(&ScanOptions {
                pi_home,
                project,
                npm_roots,
            });
            if json {
                crate::output::stdout_multiline(serde_json::to_string_pretty(&report)?);
            } else {
                print_human_report(&report, summary);
            }
            Ok(())
        }
        MigrationCommand::Import { command } => {
            migration_import::run_import(command, invocation_cwd)
        }
        MigrationCommand::Restore { backup, yes } => {
            migration_import::run_restore(backup, yes, invocation_cwd)
        }
        MigrationCommand::Adapter { command } => migration_import::run_adapter(command),
    }
}

fn default_pi_home() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("PI_CODING_AGENT_DIR") {
        return absolute_path(Path::new(&path), &std::env::current_dir()?);
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home directory is unavailable"))?;
    absolute_path(&home.join(".pi/agent"), &std::env::current_dir()?)
}

fn print_human_report(report: &MigrationReport, summary_only: bool) {
    crate::output::stdout_line("Pi migration dry run");
    crate::output::stdout_line("");
    crate::output::stdout_line("Sources:");
    for settings in &report.settings {
        let state = if settings.found { "found" } else { "not found" };
        crate::output::stdout_line(format!(
            "  {} settings: {} ({state})",
            settings.scope.label(),
            settings.path.display()
        ));
    }
    crate::output::stdout_line("");
    crate::output::stdout_line("Found:");
    crate::output::stdout_line(format!("  {} packages", report.found.packages));
    crate::output::stdout_line(format!("  {} extensions", report.found.extensions));
    crate::output::stdout_line(format!("  {} skills", report.found.skills));
    crate::output::stdout_line(format!("  {} prompts", report.found.prompts));
    crate::output::stdout_line(format!("  {} themes", report.found.themes));
    if report.found.disabled > 0 {
        crate::output::stdout_line(format!("  {} disabled resources", report.found.disabled));
    }
    crate::output::stdout_line("");
    crate::output::stdout_line("Migration:");
    for migration in [
        MigrationPath::Direct,
        MigrationPath::Replace,
        MigrationPath::Bridge,
        MigrationPath::NativePort,
        MigrationPath::Manual,
        MigrationPath::Blocked,
    ] {
        crate::output::stdout_line(format!(
            "  {:<11} {} items",
            migration.label(),
            report.migration.get(migration)
        ));
    }

    if !summary_only && !report.extensions.is_empty() {
        crate::output::stdout_line("");
        crate::output::stdout_line("Local extensions:");
        for extension in &report.extensions {
            crate::output::stdout_line(format!(
                "  {} -> {}",
                extension.path.display(),
                extension.migration.label()
            ));
            for reason in &extension.reasons {
                crate::output::stdout_line(format!("    - {reason}"));
            }
        }
    }

    if !summary_only && !report.packages.is_empty() {
        crate::output::stdout_line("");
        crate::output::stdout_line("Packages:");
        for package in &report.packages {
            let version = package.version.as_deref().unwrap_or("version unknown");
            let disabled = if !package.resources.is_empty()
                && package.resources.iter().all(|resource| !resource.enabled)
            {
                " [all resources disabled]"
            } else {
                ""
            };
            crate::output::stdout_line(format!(
                "  {} ({version}) -> {}{disabled}",
                package.source,
                package.migration.label()
            ));
            for extension in &package.extensions {
                crate::output::stdout_line(format!(
                    "    {} -> {}",
                    extension.path.display(),
                    extension.migration.label()
                ));
                for reason in &extension.reasons {
                    crate::output::stdout_line(format!("      - {reason}"));
                }
            }
        }
    }

    let diagnostic_count = report.diagnostics.len()
        + report
            .packages
            .iter()
            .map(|package| package.diagnostics.len())
            .sum::<usize>();
    if diagnostic_count > 0 {
        crate::output::stdout_line("");
        crate::output::stdout_line(format!("Diagnostics ({diagnostic_count}):"));
        for diagnostic in report.diagnostics.iter().chain(
            report
                .packages
                .iter()
                .flat_map(|package| &package.diagnostics),
        ) {
            let path = diagnostic
                .path
                .as_ref()
                .map(|path| format!(" [{}]", path.display()))
                .unwrap_or_default();
            crate::output::stdout_line(format!(
                "  {:?} {}: {}{path}",
                diagnostic.level, diagnostic.code, diagnostic.message
            ));
        }
    }

    crate::output::stdout_line("");
    crate::output::stdout_line("Estimated model usage: 0 tokens");
    crate::output::stdout_line("No files changed and no Pi package code executed.");
}

fn scan_pi(options: &ScanOptions) -> MigrationReport {
    let global_settings_path = options.pi_home.join("settings.json");
    let project_base = options.project.join(".pi");
    let project_settings_path = project_base.join("settings.json");
    let mut diagnostics = Vec::new();

    let (global_settings, global_found) =
        read_settings(&global_settings_path, Scope::User, &mut diagnostics);
    let (project_settings, project_found) =
        read_settings(&project_settings_path, Scope::Project, &mut diagnostics);

    let mut resolved = BTreeMap::<String, ResolvedPackageSetting>::new();
    resolve_settings_packages(
        &global_settings,
        Scope::User,
        &options.pi_home,
        options,
        &mut resolved,
        &mut diagnostics,
    );
    resolve_settings_packages(
        &project_settings,
        Scope::Project,
        &project_base,
        options,
        &mut resolved,
        &mut diagnostics,
    );

    let mut analysis_budget = AnalysisBudget::default();
    let mut packages = resolved
        .into_values()
        .map(|package| scan_package(package, &mut analysis_budget))
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.identity.cmp(&right.identity));

    let mut resources = Vec::new();
    let mut extensions = Vec::new();
    collect_top_level_resources(
        &options.pi_home,
        Scope::User,
        &global_settings,
        &mut resources,
        &mut extensions,
        &mut analysis_budget,
        &mut diagnostics,
    );
    collect_top_level_resources(
        &project_base,
        Scope::Project,
        &project_settings,
        &mut resources,
        &mut extensions,
        &mut analysis_budget,
        &mut diagnostics,
    );
    resources.sort_by(|left, right| (left.kind, &left.path).cmp(&(right.kind, &right.path)));
    resources.dedup_by(|left, right| left.kind == right.kind && left.path == right.path);
    extensions.sort_by(|left, right| left.path.cmp(&right.path));

    let mut found = ResourceCounts {
        packages: packages.len(),
        ..ResourceCounts::default()
    };
    let mut migration = MigrationCounts::default();
    for resource in resources
        .iter()
        .chain(packages.iter().flat_map(|package| &package.resources))
    {
        if !resource.enabled {
            found.disabled += 1;
            continue;
        }
        match resource.kind {
            ResourceKind::Extension => found.extensions += 1,
            ResourceKind::Skill => found.skills += 1,
            ResourceKind::Prompt => found.prompts += 1,
            ResourceKind::Theme => found.themes += 1,
        }
        migration.add(resource.migration);
    }
    for package in &packages {
        if package.resources.is_empty() && package.migration == MigrationPath::Blocked {
            migration.add(MigrationPath::Blocked);
        }
    }
    cap_diagnostics(
        &mut diagnostics,
        MAX_REPORT_DIAGNOSTICS,
        Some(options.project.clone()),
    );

    MigrationReport {
        schema_version: 1,
        source: "pi",
        mode: "dry_run",
        model_usage: "disabled",
        package_code_executed: false,
        settings: vec![
            SettingsReport {
                scope: Scope::User,
                path: global_settings_path,
                found: global_found,
            },
            SettingsReport {
                scope: Scope::Project,
                path: project_settings_path,
                found: project_found,
            },
        ],
        found,
        migration,
        packages,
        extensions,
        resources,
        diagnostics,
    }
}

fn read_settings(
    path: &Path,
    scope: Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> (PiSettings, bool) {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (PiSettings::default(), false);
        }
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                "settings_metadata",
                format!("could not inspect {} settings: {error}", scope.label()),
                Some(path.to_path_buf()),
            ));
            return (PiSettings::default(), false);
        }
    };
    if !metadata.file_type().is_file() {
        diagnostics.push(Diagnostic::error(
            "settings_not_regular",
            format!(
                "{} settings must be a regular, non-symlink file",
                scope.label()
            ),
            Some(path.to_path_buf()),
        ));
        return (PiSettings::default(), true);
    }
    let bytes = match octet_agent::secure_fs::read_regular_file_bounded(path, MAX_SETTINGS_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                "settings_read",
                format!("could not read {} settings: {error}", scope.label()),
                Some(path.to_path_buf()),
            ));
            return (PiSettings::default(), true);
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(settings) => (settings, true),
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                "settings_json",
                format!("invalid {} settings JSON: {error}", scope.label()),
                Some(path.to_path_buf()),
            ));
            (PiSettings::default(), true)
        }
    }
}

fn resolve_settings_packages(
    settings: &PiSettings,
    scope: Scope,
    settings_base: &Path,
    options: &ScanOptions,
    resolved: &mut BTreeMap<String, ResolvedPackageSetting>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for setting in &settings.packages {
        let source = setting.source().trim().to_owned();
        let mut package = resolve_package_setting(
            &source,
            setting.filter().cloned(),
            scope,
            settings_base,
            options,
        );
        if scope == Scope::Project
            && package
                .filter
                .as_ref()
                .is_some_and(|filter| filter.autoload == Some(false))
        {
            if let Some(user_package) = resolved.get(&package.identity) {
                package.root.clone_from(&user_package.root);
                package
                    .single_extension
                    .clone_from(&user_package.single_extension);
                package
                    .resolution_diagnostic
                    .clone_from(&user_package.resolution_diagnostic);
            }
        }
        if !resolved.contains_key(&package.identity) && resolved.len() >= MAX_PACKAGES {
            diagnostics.push(Diagnostic::error(
                "package_limit",
                format!("Pi setup exceeds the {MAX_PACKAGES}-package scan limit"),
                Some(settings_base.join("settings.json")),
            ));
            break;
        }
        // Pi gives a project package precedence over the same user package.
        // Processing the project settings second mirrors that deterministic rule.
        resolved.insert(package.identity.clone(), package);
    }
}

fn resolve_package_setting(
    source: &str,
    filter: Option<PackageFilter>,
    scope: Scope,
    settings_base: &Path,
    options: &ScanOptions,
) -> ResolvedPackageSetting {
    if let Some(spec) = source.strip_prefix("npm:") {
        return resolve_npm_package(source, spec, filter, scope, options);
    }
    if let Some((host, repository)) = parse_git_source(source) {
        let root_base = match scope {
            Scope::User => options.pi_home.join("git"),
            Scope::Project => options.project.join(".pi/git"),
        };
        let root = root_base.join(&host).join(&repository);
        return resolved_root(
            format!("git:{host}/{}", repository.display()),
            source,
            filter,
            scope,
            root,
        );
    }

    let path = match expand_local_path(source, settings_base) {
        Ok(path) => path,
        Err(error) => {
            return ResolvedPackageSetting {
                identity: format!("local:{source}"),
                source: source.to_owned(),
                scope,
                root: None,
                single_extension: None,
                filter,
                resolution_diagnostic: Some(Diagnostic::error(
                    "package_path",
                    error.to_string(),
                    None,
                )),
            };
        }
    };
    let metadata = std::fs::symlink_metadata(&path);
    match metadata {
        Ok(metadata) if metadata.file_type().is_file() => {
            let canonical = match std::fs::canonicalize(&path)
                .ok()
                .and_then(|path| normalize_absolute(&path).ok())
            {
                Some(path) => path,
                None => {
                    return unresolved_local_package(
                        source,
                        filter,
                        scope,
                        path,
                        "local extension path could not be canonicalized",
                    );
                }
            };
            ResolvedPackageSetting {
                identity: format!("local:{}", canonical.display()),
                source: source.to_owned(),
                scope,
                root: canonical.parent().map(Path::to_path_buf),
                single_extension: Some(canonical),
                filter,
                resolution_diagnostic: None,
            }
        }
        Ok(metadata) if metadata.file_type().is_dir() => {
            let canonical = match std::fs::canonicalize(&path)
                .ok()
                .and_then(|path| normalize_absolute(&path).ok())
            {
                Some(path) => path,
                None => {
                    return unresolved_local_package(
                        source,
                        filter,
                        scope,
                        path,
                        "local package directory could not be canonicalized",
                    );
                }
            };
            resolved_root(
                format!("local:{}", canonical.display()),
                source,
                filter,
                scope,
                canonical,
            )
        }
        _ => resolved_root(
            format!("local:{}", path.display()),
            source,
            filter,
            scope,
            path,
        ),
    }
}

fn unresolved_local_package(
    source: &str,
    filter: Option<PackageFilter>,
    scope: Scope,
    path: PathBuf,
    message: &'static str,
) -> ResolvedPackageSetting {
    ResolvedPackageSetting {
        identity: format!("local:{}", path.display()),
        source: source.to_owned(),
        scope,
        root: None,
        single_extension: None,
        filter,
        resolution_diagnostic: Some(Diagnostic::error("package_unresolved", message, Some(path))),
    }
}

fn resolve_npm_package(
    source: &str,
    spec: &str,
    filter: Option<PackageFilter>,
    scope: Scope,
    options: &ScanOptions,
) -> ResolvedPackageSetting {
    let Some(name) = npm_package_name(spec) else {
        return ResolvedPackageSetting {
            identity: format!("npm:{spec}"),
            source: source.to_owned(),
            scope,
            root: None,
            single_extension: None,
            filter,
            resolution_diagnostic: Some(Diagnostic::error(
                "npm_spec",
                format!("invalid npm package source {source:?}"),
                None,
            )),
        };
    };
    let mut roots = Vec::new();
    match scope {
        Scope::User => roots.push(options.pi_home.join("npm/node_modules")),
        Scope::Project => roots.push(options.project.join(".pi/npm/node_modules")),
    }
    if scope == Scope::User {
        roots.extend(options.npm_roots.iter().cloned());
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".local/lib/node_modules"));
        }
        roots.push(PathBuf::from("/usr/local/lib/node_modules"));
        roots.push(PathBuf::from("/opt/homebrew/lib/node_modules"));
    }
    let mut seen_roots = BTreeSet::new();
    roots.retain(|root| seen_roots.insert(root.clone()));
    let candidates = roots
        .iter()
        .map(|root| root.join(&name))
        .collect::<Vec<_>>();
    let selected = candidates
        .iter()
        .find(|path| std::fs::symlink_metadata(path).is_ok())
        .cloned()
        .or_else(|| candidates.first().cloned());
    let Some(root) = selected else {
        return ResolvedPackageSetting {
            identity: format!("npm:{name}"),
            source: source.to_owned(),
            scope,
            root: None,
            single_extension: None,
            filter,
            resolution_diagnostic: Some(Diagnostic::error(
                "npm_root",
                "no npm installation root is available",
                None,
            )),
        };
    };
    resolved_root(format!("npm:{name}"), source, filter, scope, root)
}

fn resolved_root(
    identity: String,
    source: &str,
    filter: Option<PackageFilter>,
    scope: Scope,
    root: PathBuf,
) -> ResolvedPackageSetting {
    let resolution_diagnostic = match validate_directory_root(&root) {
        Ok(()) => None,
        Err(error) => Some(Diagnostic::error(
            "package_unresolved",
            error,
            Some(root.clone()),
        )),
    };
    ResolvedPackageSetting {
        identity,
        source: source.to_owned(),
        scope,
        root: resolution_diagnostic.is_none().then_some(root),
        single_extension: None,
        filter,
        resolution_diagnostic,
    }
}

fn scan_package(
    package: ResolvedPackageSetting,
    analysis_budget: &mut AnalysisBudget,
) -> PackageReport {
    let mut diagnostics = Vec::new();
    if let Some(diagnostic) = package.resolution_diagnostic {
        diagnostics.push(diagnostic);
    }
    let Some(root) = package.root.as_ref() else {
        return PackageReport {
            identity: package.identity,
            source: package.source,
            scope: package.scope,
            root: None,
            name: None,
            version: None,
            source_hash: None,
            lock_hash: None,
            migration: MigrationPath::Blocked,
            resources: Vec::new(),
            extensions: Vec::new(),
            diagnostics,
        };
    };

    let package_json_path = root.join("package.json");
    let single_extension = package.single_extension.is_some();
    let package_json = if single_extension {
        None
    } else {
        read_package_json(&package_json_path, &mut diagnostics)
    };
    let mut resources = if let Some(extension) = package.single_extension {
        vec![ResourceReport {
            kind: ResourceKind::Extension,
            scope: package.scope,
            path: extension,
            enabled: true,
            migration: MigrationPath::Bridge,
        }]
    } else {
        collect_package_resources(
            root,
            package_json.as_ref().and_then(|json| json.pi.as_ref()),
            package.filter.as_ref(),
            package.scope,
            &mut diagnostics,
        )
    };
    let resource_remaining = MAX_SCAN_RESOURCES.saturating_sub(analysis_budget.resources);
    if resources.len() > resource_remaining {
        diagnostics.push(Diagnostic::warning(
            "scan_resource_limit",
            format!("setup resource inventory stopped at {MAX_SCAN_RESOURCES} entries"),
            Some(root.clone()),
        ));
        resources.truncate(resource_remaining);
    }
    analysis_budget.resources = analysis_budget.resources.saturating_add(resources.len());

    let mut extensions = Vec::new();
    let mut analyzed_files = BTreeSet::new();
    for resource in &mut resources {
        if resource.kind != ResourceKind::Extension || !resource.enabled {
            continue;
        }
        let report =
            analyze_extension_with_budget(&resource.path, root, analysis_budget, &mut diagnostics);
        resource.migration = report.migration;
        analyzed_files.extend(report.analyzed_files.iter().cloned());
        extensions.push(report);
    }
    resources.sort_by(|left, right| (left.kind, &left.path).cmp(&(right.kind, &right.path)));

    let mut source_files = resources
        .iter()
        .map(|resource| resource.path.clone())
        .collect::<BTreeSet<_>>();
    source_files.extend(analyzed_files);
    if !single_extension {
        source_files.extend(
            walk_regular_files(root, &mut diagnostics)
                .into_iter()
                .filter(|path| is_package_source_file(path)),
        );
    }
    if !single_extension && std::fs::symlink_metadata(&package_json_path).is_ok() {
        source_files.insert(package_json_path);
    }
    let source_hash = hash_files(
        root,
        source_files.iter().map(PathBuf::as_path),
        MAX_PACKAGE_SOURCE_BYTES,
        "package source",
        analysis_budget,
        &mut diagnostics,
    );
    let lock_names = [
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
    ];
    let mut lock_root: &Path = root;
    let mut lock_files = if single_extension {
        Vec::new()
    } else {
        lock_names
            .iter()
            .map(|name| root.join(name))
            .filter(|path| std::fs::symlink_metadata(path).is_ok())
            .collect::<Vec<_>>()
    };
    if lock_files.is_empty() && package.identity.starts_with("npm:") {
        if let Some(install_root) = root
            .ancestors()
            .find(|ancestor| ancestor.file_name() == Some(OsStr::new("node_modules")))
            .and_then(Path::parent)
        {
            lock_root = install_root;
            lock_files = lock_names
                .iter()
                .map(|name| install_root.join(name))
                .filter(|path| std::fs::symlink_metadata(path).is_ok())
                .collect();
        }
    }
    let lock_hash = if lock_files.is_empty() {
        None
    } else {
        hash_files(
            lock_root,
            lock_files.iter().map(PathBuf::as_path),
            MAX_LOCK_FILE_BYTES,
            "package lock",
            analysis_budget,
            &mut diagnostics,
        )
    };

    let migration = resources
        .iter()
        .filter(|resource| resource.enabled)
        .map(|resource| resource.migration)
        .max()
        .or_else(|| resources.iter().map(|resource| resource.migration).max())
        .unwrap_or(MigrationPath::Blocked);
    cap_diagnostics(
        &mut diagnostics,
        MAX_PACKAGE_DIAGNOSTICS,
        Some(root.clone()),
    );
    PackageReport {
        identity: package.identity,
        source: package.source,
        scope: package.scope,
        root: Some(root.clone()),
        name: package_json.as_ref().and_then(|json| json.name.clone()),
        version: package_json.as_ref().and_then(|json| json.version.clone()),
        source_hash,
        lock_hash,
        migration,
        resources,
        extensions,
        diagnostics,
    }
}

fn read_package_json(path: &Path, diagnostics: &mut Vec<Diagnostic>) -> Option<PackageJson> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                "package_json_metadata",
                format!("could not inspect package.json: {error}"),
                Some(path.to_path_buf()),
            ));
            return None;
        }
    };
    if !metadata.file_type().is_file() {
        diagnostics.push(Diagnostic::error(
            "package_json_not_regular",
            "package.json must be a regular, non-symlink file",
            Some(path.to_path_buf()),
        ));
        return None;
    }
    let bytes =
        match octet_agent::secure_fs::read_regular_file_bounded(path, MAX_PACKAGE_JSON_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(Diagnostic::error(
                    "package_json_read",
                    format!("could not read package.json: {error}"),
                    Some(path.to_path_buf()),
                ));
                return None;
            }
        };
    match serde_json::from_slice(&bytes) {
        Ok(package) => Some(package),
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                "package_json_invalid",
                format!("invalid package.json: {error}"),
                Some(path.to_path_buf()),
            ));
            None
        }
    }
}

fn hash_files<'a>(
    root: &Path,
    files: impl IntoIterator<Item = &'a Path>,
    total_limit: usize,
    label: &str,
    analysis_budget: &mut AnalysisBudget,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let mut files = files.into_iter().map(Path::to_path_buf).collect::<Vec<_>>();
    files.sort();
    files.dedup();
    let mut total = 0usize;
    let mut hasher = Sha256::new();
    let mut hashed = 0usize;
    let mut complete = true;
    for path in files {
        let package_remaining = total_limit.saturating_sub(total);
        let scan_remaining = MAX_SCAN_HASH_BYTES.saturating_sub(analysis_budget.hashed_bytes);
        let remaining = package_remaining.min(scan_remaining);
        if remaining == 0 {
            diagnostics.push(Diagnostic::warning(
                "hash_limit",
                format!("{label} hashing exhausted its package or setup byte budget"),
                Some(root.to_path_buf()),
            ));
            complete = false;
            break;
        }
        let per_file_limit = if path.file_name().is_some_and(|name| {
            matches!(
                name.to_str(),
                Some("package-lock.json" | "npm-shrinkwrap.json" | "pnpm-lock.yaml" | "yarn.lock")
            )
        }) {
            MAX_LOCK_FILE_BYTES.min(remaining)
        } else {
            MAX_SOURCE_FILE_BYTES.min(remaining)
        };
        let bytes = match octet_agent::secure_fs::read_regular_file_bounded(&path, per_file_limit) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(Diagnostic::warning(
                    "hash_read",
                    format!("could not include file in {label} hash: {error}"),
                    Some(path),
                ));
                complete = false;
                continue;
            }
        };
        total += bytes.len();
        analysis_budget.hashed_bytes = analysis_budget.hashed_bytes.saturating_add(bytes.len());
        let relative = relative_slash(root, &path);
        hasher.update((relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
        hashed += 1;
    }
    (complete && hashed > 0).then(|| format!("{:x}", hasher.finalize()))
}

fn npm_package_name(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let name = if spec.starts_with('@') {
        let slash = spec.find('/')?;
        match spec[slash + 1..].rfind('@') {
            Some(offset) => &spec[..slash + 1 + offset],
            None => spec,
        }
    } else {
        spec.rsplit_once('@').map_or(spec, |(name, _)| name)
    };
    let valid = !name.is_empty()
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "@._~-".contains(character)
                })
        });
    valid.then(|| name.to_owned())
}

fn parse_git_source(source: &str) -> Option<(String, PathBuf)> {
    let trimmed = source.trim();
    let explicit = trimmed.strip_prefix("git:");
    let value = explicit.unwrap_or(trimmed);
    let (host, path) = if let Some(rest) = value.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        (host.to_owned(), path.to_owned())
    } else if value.contains("://") {
        let url = url::Url::parse(value).ok()?;
        (
            url.host_str()?.to_owned(),
            url.path().trim_start_matches('/').to_owned(),
        )
    } else if explicit.is_some() {
        let (host, path) = value.split_once('/')?;
        (host.to_owned(), path.to_owned())
    } else {
        return None;
    };
    let path = strip_git_ref(&path).trim_end_matches(".git");
    let components = path.split('/').collect::<Vec<_>>();
    let valid = !host.is_empty()
        && components.len() >= 2
        && components.iter().all(|component| {
            !component.is_empty()
                && *component != "."
                && *component != ".."
                && !component.contains(['\\', '\0'])
        });
    valid.then(|| (host, components.into_iter().collect()))
}

fn strip_git_ref(path: &str) -> &str {
    path.rsplit_once('@').map_or(path, |(path, _)| path)
}

fn expand_local_path(source: &str, base: &Path) -> anyhow::Result<PathBuf> {
    let path = if source == "~" {
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home directory is unavailable"))?
    } else if let Some(path) = source.strip_prefix("~/") {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("home directory is unavailable"))?
            .join(path)
    } else {
        let path = Path::new(source);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        }
    };
    normalize_absolute(&path)
}

fn absolute_path(path: &Path, cwd: &Path) -> anyhow::Result<PathBuf> {
    let path = if path.is_absolute() {
        normalize_absolute(path)?
    } else {
        normalize_absolute(&cwd.join(path))?
    };
    let resolved = match std::fs::canonicalize(&path) {
        Ok(canonical) => normalize_absolute(&canonical)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path,
        Err(error) => return Err(error.into()),
    };
    if resolved.to_str().is_none() {
        anyhow::bail!(
            "migration paths must be valid UTF-8: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

fn normalize_absolute(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("path must be absolute: {}", path.display());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    anyhow::bail!("path escapes its filesystem root: {}", path.display());
                }
            }
            Component::Normal(component) => normalized.push(component),
        }
    }
    Ok(normalized)
}

fn confined_join(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        anyhow::bail!("package resource path must be relative: {relative:?}");
    }
    let path = normalize_absolute(&root.join(candidate))?;
    if !path.starts_with(root) {
        anyhow::bail!("package resource path escapes its package: {relative:?}");
    }
    Ok(path)
}

fn validate_directory_root(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("package directory is unavailable: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("symlinked package directories are not scanned".to_owned());
    }
    if !metadata.is_dir() {
        return Err("package path is not a directory".to_owned());
    }
    let normalized = normalize_absolute(path).map_err(|error| error.to_string())?;
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("package directory could not be canonicalized: {error}"))?;
    let canonical = normalize_absolute(&canonical).map_err(|error| error.to_string())?;
    if normalized != canonical {
        return Err("package directory traverses a symbolic link".to_owned());
    }
    Ok(())
}

fn has_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', '{'])
}

fn compile_glob(pattern: &str) -> Result<GlobMatcher, globset::Error> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .map(|glob| glob.compile_matcher())
}

fn relative_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(component) => Some(component.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests;

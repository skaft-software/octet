//! Resource collection for the legacy `pi` migration.
//!
//! Why this is a separate module: enumerating what a package actually ships is a
//! different question from deciding where each piece has to go, and from reading
//! the source of an extension once it has been found. This module owns the
//! directory-walking half of the scanner - the manifest, filter and pattern
//! narrowing, and the bounded walk that produces the candidate skills, prompts,
//! themes and extensions - so that `migrate` itself is left with report shapes
//! and rendering, and [`super::scan_pi`] is left reading as the orchestration
//! over these two halves.
//!
//! Every walk here is bounded: see `MAX_WALK_ENTRIES`, `MAX_RESOURCE_FILES` and
//! `MAX_EXTENSION_SOURCE_BYTES` in the parent module.

use ignore::WalkBuilder;

use super::source_analysis::analyze_extension_with_budget;
use super::*;

pub(super) fn collect_package_resources(
    root: &Path,
    manifest: Option<&PiManifest>,
    filter: Option<&PackageFilter>,
    scope: Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ResourceReport> {
    let mut reports = Vec::new();
    let mut traversal = ResourceTraversal::default();
    for kind in ResourceKind::ALL {
        let mut paths = match manifest {
            Some(manifest) => match manifest.entries(kind) {
                Some(entries) => {
                    collect_manifest_entries(root, entries, kind, &mut traversal, diagnostics)
                }
                None if filter.is_some() => collect_resource_path(
                    &root.join(kind.key()),
                    kind,
                    root,
                    &mut traversal,
                    diagnostics,
                ),
                None => Vec::new(),
            },
            None => collect_resource_path(
                &root.join(kind.key()),
                kind,
                root,
                &mut traversal,
                diagnostics,
            ),
        };
        paths.sort();
        paths.dedup();
        let enabled = apply_filter(
            root,
            &paths,
            filter.and_then(|filter| filter.patterns(kind)),
            filter.and_then(|filter| filter.autoload).unwrap_or(true),
            diagnostics,
        );
        reports.extend(paths.into_iter().map(|path| {
            let enabled = enabled.contains(&path);
            ResourceReport {
                kind,
                scope,
                path,
                enabled,
                migration: default_migration(kind),
            }
        }));
    }
    reports
}

fn collect_manifest_entries(
    root: &Path,
    entries: &[String],
    kind: ResourceKind,
    traversal: &mut ResourceTraversal,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let tracks_extension_manifest = kind == ResourceKind::Extension;
    if tracks_extension_manifest {
        if traversal.extension_manifest_resolution_stopped {
            return Vec::new();
        }
        if traversal.active_extension_manifests.len() >= MAX_EXTENSION_MANIFEST_DEPTH {
            traversal.extension_manifest_resolution_stopped = true;
            diagnostics.push(Diagnostic::warning(
                "manifest_depth_limit",
                format!(
                    "extension manifest resolution stopped at {MAX_EXTENSION_MANIFEST_DEPTH} nested directories"
                ),
                Some(root.to_path_buf()),
            ));
            return Vec::new();
        }
        if traversal.extension_manifest_is_active(root) {
            traversal.extension_manifest_resolution_stopped = true;
            diagnostics.push(Diagnostic::warning(
                "manifest_cycle",
                "extension manifest resolution revisited an active directory",
                Some(root.to_path_buf()),
            ));
            return Vec::new();
        }
        traversal
            .active_extension_manifests
            .push(root.to_path_buf());
    }

    let mut paths = Vec::new();
    if entries.len() > MAX_PATTERNS {
        diagnostics.push(Diagnostic::warning(
            "manifest_entry_limit",
            format!("only the first {MAX_PATTERNS} manifest entries were inspected"),
            Some(root.to_path_buf()),
        ));
    }
    for entry in entries
        .iter()
        .take(MAX_PATTERNS)
        .filter(|entry| !entry.starts_with(['!', '+', '-']))
    {
        if has_glob(entry) {
            let matcher = match compile_glob(entry) {
                Ok(matcher) => matcher,
                Err(error) => {
                    diagnostics.push(Diagnostic::warning(
                        "manifest_glob",
                        format!("invalid {} pattern {entry:?}: {error}", kind.key()),
                        Some(root.to_path_buf()),
                    ));
                    continue;
                }
            };
            for candidate in walk_regular_files(root, diagnostics) {
                let relative = relative_slash(root, &candidate);
                if matcher.is_match(&relative) {
                    paths.extend(collect_resource_path(
                        &candidate,
                        kind,
                        root,
                        traversal,
                        diagnostics,
                    ));
                }
            }
        } else {
            match confined_join(root, entry) {
                Ok(path) => paths.extend(collect_resource_path(
                    &path,
                    kind,
                    root,
                    traversal,
                    diagnostics,
                )),
                Err(error) => diagnostics.push(Diagnostic::warning(
                    "manifest_path",
                    error.to_string(),
                    Some(root.to_path_buf()),
                )),
            }
        }
        if paths.len() >= MAX_RESOURCE_FILES {
            diagnostics.push(Diagnostic::error(
                "resource_limit",
                format!("package exceeds the {MAX_RESOURCE_FILES}-resource scan limit"),
                Some(root.to_path_buf()),
            ));
            paths.truncate(MAX_RESOURCE_FILES);
            break;
        }
    }
    let paths = apply_manifest_overrides(root, paths, entries, diagnostics);
    if tracks_extension_manifest {
        let popped = traversal.active_extension_manifests.pop();
        debug_assert_eq!(popped.as_deref(), Some(root));
    }
    paths
}

fn apply_manifest_overrides(
    root: &Path,
    mut paths: Vec<PathBuf>,
    entries: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let patterns = entries
        .iter()
        .take(MAX_PATTERNS)
        .filter(|entry| entry.starts_with(['!', '+', '-']))
        .cloned()
        .collect::<Vec<_>>();
    if patterns.is_empty() {
        return paths;
    }
    let original = paths.clone();
    let enabled = apply_patterns(root, &original, &patterns, true, diagnostics);
    paths.retain(|path| enabled.contains(path));
    paths
}

fn apply_filter(
    root: &Path,
    paths: &[PathBuf],
    patterns: Option<&[String]>,
    autoload: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeSet<PathBuf> {
    match patterns {
        None if autoload => paths.iter().cloned().collect(),
        None => BTreeSet::new(),
        Some(patterns) => apply_patterns(root, paths, patterns, autoload, diagnostics),
    }
}

fn apply_patterns(
    root: &Path,
    paths: &[PathBuf],
    patterns: &[String],
    default_include: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeSet<PathBuf> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    let mut force_includes = Vec::new();
    let mut force_excludes = Vec::new();
    if patterns.len() > MAX_PATTERNS {
        diagnostics.push(Diagnostic::warning(
            "pattern_limit",
            format!("only the first {MAX_PATTERNS} resource patterns were inspected"),
            Some(root.to_path_buf()),
        ));
    }
    for pattern in patterns.iter().take(MAX_PATTERNS) {
        let (target, value) = if let Some(value) = pattern.strip_prefix('+') {
            (&mut force_includes, value)
        } else if let Some(value) = pattern.strip_prefix('-') {
            (&mut force_excludes, value)
        } else if let Some(value) = pattern.strip_prefix('!') {
            (&mut excludes, value)
        } else {
            (&mut includes, pattern.as_str())
        };
        match compile_glob(value) {
            Ok(matcher) => target.push((value.to_owned(), matcher)),
            Err(error) => diagnostics.push(Diagnostic::warning(
                "filter_glob",
                format!("invalid package filter {pattern:?}: {error}"),
                Some(root.to_path_buf()),
            )),
        }
    }
    let include_by_default = includes.is_empty() && default_include;
    let mut selected = BTreeSet::new();
    for path in paths {
        let relative = relative_slash(root, path);
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let skill_parent = (name == "SKILL.md")
            .then(|| path.parent().map(|parent| relative_slash(root, parent)))
            .flatten();
        let matches = |patterns: &[(String, GlobMatcher)]| {
            patterns.iter().any(|(raw, matcher)| {
                matcher.is_match(&relative)
                    || matcher.is_match(name)
                    || skill_parent
                        .as_deref()
                        .is_some_and(|parent| matcher.is_match(parent))
                    || (!has_glob(raw) && raw.trim_start_matches("./") == relative)
            })
        };
        let mut enabled = include_by_default || matches(&includes);
        if matches(&excludes) {
            enabled = false;
        }
        if matches(&force_includes) {
            enabled = true;
        }
        if matches(&force_excludes) {
            enabled = false;
        }
        if enabled {
            selected.insert(path.clone());
        }
    }
    selected
}

pub(super) fn collect_top_level_resources(
    base: &Path,
    scope: Scope,
    settings: &PiSettings,
    reports: &mut Vec<ResourceReport>,
    extensions: &mut Vec<ExtensionReport>,
    analysis_budget: &mut AnalysisBudget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut traversal = ResourceTraversal::default();
    for kind in ResourceKind::ALL {
        let mut paths = collect_resource_path(
            &base.join(kind.key()),
            kind,
            base,
            &mut traversal,
            diagnostics,
        );
        let resource_remaining = MAX_SCAN_RESOURCES.saturating_sub(analysis_budget.resources);
        if paths.len() > resource_remaining {
            diagnostics.push(Diagnostic::warning(
                "scan_resource_limit",
                format!("setup resource inventory stopped at {MAX_SCAN_RESOURCES} entries"),
                Some(base.to_path_buf()),
            ));
            paths.truncate(resource_remaining);
        }
        analysis_budget.resources = analysis_budget.resources.saturating_add(paths.len());
        if settings.overrides(kind).len() > MAX_PATTERNS {
            diagnostics.push(Diagnostic::warning(
                "pattern_limit",
                format!("only the first {MAX_PATTERNS} top-level overrides were inspected"),
                Some(base.join("settings.json")),
            ));
        }
        let overrides = settings
            .overrides(kind)
            .iter()
            .take(MAX_PATTERNS)
            .filter(|pattern| pattern.starts_with(['!', '+', '-']))
            .cloned()
            .collect::<Vec<_>>();
        let enabled = apply_patterns(base, &paths, &overrides, true, diagnostics);
        for path in paths {
            let enabled = enabled.contains(&path);
            let mut migration = default_migration(kind);
            if kind == ResourceKind::Extension && enabled {
                let extension =
                    analyze_extension_with_budget(&path, base, analysis_budget, diagnostics);
                migration = extension.migration;
                extensions.push(extension);
            }
            reports.push(ResourceReport {
                kind,
                scope,
                enabled,
                path,
                migration,
            });
        }
    }
}

pub(super) fn collect_resource_path(
    path: &Path,
    kind: ResourceKind,
    package_root: &Path,
    traversal: &mut ResourceTraversal,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    if kind == ResourceKind::Extension && traversal.extension_manifest_resolution_stopped {
        return Vec::new();
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                "resource_metadata",
                format!("could not inspect {} resource: {error}", kind.key()),
                Some(path.to_path_buf()),
            ));
            return Vec::new();
        }
    };
    if metadata.file_type().is_symlink() {
        diagnostics.push(Diagnostic::warning(
            "resource_symlink",
            "symlinked resources are not read by the migration scanner",
            Some(path.to_path_buf()),
        ));
        return Vec::new();
    }
    if metadata.is_file() {
        return is_resource_file(path, kind)
            .then(|| path.to_path_buf())
            .into_iter()
            .collect();
    }
    if !metadata.is_dir() {
        diagnostics.push(Diagnostic::warning(
            "resource_not_regular",
            "resource entry is neither a regular file nor directory",
            Some(path.to_path_buf()),
        ));
        return Vec::new();
    }

    match kind {
        ResourceKind::Extension => collect_extension_directory(path, traversal, diagnostics),
        ResourceKind::Skill => collect_skill_directory(path, diagnostics),
        ResourceKind::Prompt | ResourceKind::Theme => walk_regular_files(path, diagnostics)
            .into_iter()
            .filter(|candidate| is_resource_file(candidate, kind))
            .take(MAX_RESOURCE_FILES)
            .collect(),
    }
    .into_iter()
    .filter(|candidate| candidate.starts_with(package_root))
    .collect()
}

fn collect_extension_directory(
    path: &Path,
    traversal: &mut ResourceTraversal,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    if !traversal.extension_manifest_is_active(path) {
        if let Some(entries) = extension_directory_entrypoints(path, traversal, diagnostics) {
            return entries;
        }
    }
    if let Some(entrypoint) = extension_index_entrypoint(path) {
        return vec![entrypoint];
    }
    let mut entries = Vec::new();
    let read_dir = match std::fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                "extension_directory",
                format!("could not read extension directory: {error}"),
                Some(path.to_path_buf()),
            ));
            return entries;
        }
    };
    for (visited, entry) in read_dir.flatten().enumerate() {
        if visited >= MAX_WALK_ENTRIES {
            diagnostics.push(Diagnostic::warning(
                "walk_entry_limit",
                format!("extension directory stopped at {MAX_WALK_ENTRIES} entries"),
                Some(path.to_path_buf()),
            ));
            break;
        }
        if entries.len() >= MAX_RESOURCE_FILES {
            diagnostics.push(Diagnostic::warning(
                "resource_limit",
                format!("extension discovery stopped at {MAX_RESOURCE_FILES} files"),
                Some(path.to_path_buf()),
            ));
            break;
        }
        let candidate = entry.path();
        if candidate.to_str().is_none() {
            diagnostics.push(Diagnostic::warning(
                "resource_path_utf8",
                "resource paths must be valid UTF-8",
                Some(path.to_path_buf()),
            ));
            continue;
        }
        let metadata = match entry.file_type() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_symlink() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if metadata.is_file() && is_resource_file(&candidate, ResourceKind::Extension) {
            entries.push(candidate);
        } else if metadata.is_dir() && entry.file_name() != "node_modules" {
            if let Some(mut nested) =
                extension_directory_entrypoints(&candidate, traversal, diagnostics)
            {
                entries.append(&mut nested);
            }
        }
    }
    entries
}

fn extension_directory_entrypoints(
    path: &Path,
    traversal: &mut ResourceTraversal,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<PathBuf>> {
    let package_json = read_package_json(&path.join("package.json"), diagnostics);
    if let Some(entries) = package_json
        .as_ref()
        .and_then(|package| package.pi.as_ref())
        .and_then(|manifest| manifest.extensions.as_deref())
    {
        let entries = collect_manifest_entries(
            path,
            entries,
            ResourceKind::Extension,
            traversal,
            diagnostics,
        );
        if traversal.extension_manifest_resolution_stopped || !entries.is_empty() {
            return Some(entries);
        }
    }
    extension_index_entrypoint(path).map(|entrypoint| vec![entrypoint])
}

fn extension_index_entrypoint(path: &Path) -> Option<PathBuf> {
    [
        "index.ts",
        "index.tsx",
        "index.js",
        "index.mjs",
        "index.cjs",
    ]
    .into_iter()
    .map(|name| path.join(name))
    .find(|candidate| {
        std::fs::symlink_metadata(candidate).is_ok_and(|metadata| metadata.file_type().is_file())
    })
}

fn collect_skill_directory(path: &Path, diagnostics: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    walk_regular_files(path, diagnostics)
        .into_iter()
        .filter(|candidate| {
            candidate.file_name() == Some(OsStr::new("SKILL.md"))
                || (candidate.parent() == Some(path)
                    && candidate.extension() == Some(OsStr::new("md")))
        })
        .take(MAX_RESOURCE_FILES)
        .collect()
}

pub(super) fn walk_regular_files(root: &Path, diagnostics: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    if let Err(error) = validate_directory_root(root) {
        diagnostics.push(Diagnostic::warning(
            "resource_root",
            error,
            Some(root.to_path_buf()),
        ));
        return Vec::new();
    }
    let mut files = Vec::new();
    let mut visited = 0usize;
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .follow_links(false)
        .parents(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .filter_entry(|entry| entry.file_name() != "node_modules" && entry.file_name() != ".git")
        .build();
    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                diagnostics.push(Diagnostic::warning(
                    "resource_walk",
                    format!("could not inspect resource entry: {error}"),
                    Some(root.to_path_buf()),
                ));
                continue;
            }
        };
        visited += 1;
        if visited > MAX_WALK_ENTRIES {
            diagnostics.push(Diagnostic::warning(
                "walk_entry_limit",
                format!("resource walk stopped at {MAX_WALK_ENTRIES} entries"),
                Some(root.to_path_buf()),
            ));
            break;
        }
        if entry.depth() == 0 {
            continue;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            diagnostics.push(Diagnostic::warning(
                "resource_symlink",
                "symlinked resources are not read by the migration scanner",
                Some(entry.path().to_path_buf()),
            ));
            continue;
        }
        if file_type.is_file() {
            if entry.path().to_str().is_none() {
                diagnostics.push(Diagnostic::warning(
                    "resource_path_utf8",
                    "resource paths must be valid UTF-8",
                    Some(root.to_path_buf()),
                ));
                continue;
            }
            files.push(entry.into_path());
            if files.len() >= MAX_RESOURCE_FILES {
                diagnostics.push(Diagnostic::warning(
                    "resource_limit",
                    format!("resource walk stopped at {MAX_RESOURCE_FILES} files"),
                    Some(root.to_path_buf()),
                ));
                break;
            }
        }
    }
    files
}

pub(super) fn is_package_source_file(path: &Path) -> bool {
    if path.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some("package-lock.json" | "npm-shrinkwrap.json" | "pnpm-lock.yaml" | "yarn.lock")
        )
    }) {
        return false;
    }
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some(
            "ts" | "tsx"
                | "mts"
                | "cts"
                | "js"
                | "mjs"
                | "cjs"
                | "json"
                | "md"
                | "toml"
                | "yaml"
                | "yml"
        )
    )
}

fn is_resource_file(path: &Path, kind: ResourceKind) -> bool {
    let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    match kind {
        ResourceKind::Extension => matches!(extension, "ts" | "tsx" | "js" | "mjs" | "cjs"),
        ResourceKind::Skill => {
            path.file_name() == Some(OsStr::new("SKILL.md")) || extension == "md"
        }
        ResourceKind::Prompt => extension == "md",
        ResourceKind::Theme => extension == "json",
    }
}

fn default_migration(kind: ResourceKind) -> MigrationPath {
    match kind {
        ResourceKind::Extension => MigrationPath::Bridge,
        ResourceKind::Skill | ResourceKind::Prompt => MigrationPath::Direct,
        ResourceKind::Theme => MigrationPath::Manual,
    }
}

//! Where a theme selector comes from: compiled-in names, then discovered files.
//!
//! Why this is a separate module: this is the only place in the theme stack
//! that touches a theme *name* as a lookup key rather than as a value. A
//! selector can be a compiled-in built-in (`Cards`, `Still`, `default`), a
//! discovered `*.toml` under the shared resource roots, or neither - and telling
//! those apart is a trust decision, not a formatting one. Keeping the whole
//! selector-to-source boundary here means the trust rules (stem reservation,
//! single-component name validation, no-follow bounded read) are auditable in
//! one file instead of scattered through the compile and load paths.
//!
//! The split is deliberate: a compiled-in built-in always wins over a discovered
//! file of the same stem, and the stem is *reserved* so a user's `Cards.toml`
//! can neither shadow nor be shadowed by the built-in it selects. Configuration
//! validation, the `/theme` picker, and initial load all ask this module the
//! same two questions - is this name reserved, and where does it resolve - so
//! they cannot drift apart.

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use super::{
    load_theme_source_for, CompiledFileTheme, OctetTheme, TerminalBackground, TerminalCapabilities,
    TerminalThemeChoice, ThemeSource, CARDS_THEME_NAME, CARDS_THEME_SOURCE, COMPILED_FILE_THEMES,
    DEFAULT_THEME_NAME, STILL_THEME_NAME, STILL_THEME_SOURCE,
};
use crate::config::Config;
use crate::resource_resolver::{ResourceKind, ResourceResolver};
use crate::tui::theme_schema::MAX_THEME_BYTES;

pub(crate) fn is_reserved_theme_name(name: &str) -> bool {
    is_builtin_theme_name(name) || TerminalThemeChoice::parse(name).is_some()
}

/// Whether `name` selects a compiled-in file theme such as `Cards` or `Still`.
/// Their stems are reserved against discovered files, and this is the predicate
/// that lets configuration accept the built-in under its own name.
pub(crate) fn is_compiled_file_theme_name(name: &str) -> bool {
    compiled_file_theme_name(name).is_some()
}

/// The canonical spelling of a compiled-in file theme selector, so a persisted
/// `cards.toml` or `Cards` both resolve to the one built-in name.
pub(crate) fn compiled_file_theme_name(name: &str) -> Option<&'static str> {
    let stem = name.strip_suffix(".toml").unwrap_or(name);
    COMPILED_FILE_THEMES
        .iter()
        .find(|(built_in, _)| stem.eq_ignore_ascii_case(built_in))
        .map(|(built_in, _)| *built_in)
}

/// Every selector answered by a compiled-in theme rather than a discovered
/// file. Reserving these names keeps a user's `Cards.toml` or `Still.toml` from
/// shadowing, or being shadowed by, the built-in they select.
fn is_builtin_theme_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(DEFAULT_THEME_NAME) || compiled_file_theme_for(name).is_some()
}

/// Selectors for the compiled-in file themes, in the order the `/theme` picker
/// offers them.
pub fn compiled_file_theme_names() -> impl Iterator<Item = &'static str> {
    COMPILED_FILE_THEMES.iter().map(|(name, _)| *name)
}

/// Resolve a selector, with or without a `.toml` suffix, to the loader for a
/// compiled-in file theme. Returns `None` for the compiled default and for
/// discovered-file selectors.
pub(super) fn compiled_file_theme_for(name: &str) -> Option<CompiledFileTheme> {
    let stem = name.strip_suffix(".toml").unwrap_or(name);
    COMPILED_FILE_THEMES
        .iter()
        .find(|(built_in, _)| stem.eq_ignore_ascii_case(built_in))
        .map(|(_, load)| *load)
}

/// Compile the embedded `Cards` theme for one background profile. A built-in
/// that fails to compile is a build-time defect the example test already
/// covers, so surface it as a load error rather than a silent fallback.
pub(super) fn cards_theme_for(
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    load_theme_source_for(
        CARDS_THEME_SOURCE,
        CARDS_THEME_NAME,
        ThemeSource::CompiledCards,
        CARDS_THEME_NAME,
        capabilities,
        background,
    )
}

/// Compile the embedded `Still` theme for one background profile. A built-in
/// that fails to compile is a build-time defect the example test already
/// covers, so surface it as a load error rather than a silent fallback.
pub(super) fn still_theme_for(
    capabilities: TerminalCapabilities,
    background: TerminalBackground,
) -> anyhow::Result<OctetTheme> {
    load_theme_source_for(
        STILL_THEME_SOURCE,
        STILL_THEME_NAME,
        ThemeSource::CompiledStill,
        STILL_THEME_NAME,
        capabilities,
        background,
    )
}

pub(super) fn theme_file_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || Path::new(name).components().count() != 1
        || name
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'\0'))
    {
        return None;
    }
    Some(if name.ends_with(".toml") {
        name.to_owned()
    } else {
        format!("{name}.toml")
    })
}

pub(super) fn discover_themes(config: &Config) -> crate::resource_resolver::ResourceSnapshot {
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    resolver.discover(ResourceKind::Theme, &config.theme_paths)
}

/// Return best-effort diagnostics from the theme discovery pass. A diagnostic
/// is inspectable by callers but never turns discovery into a startup error.
#[cfg(test)]
pub fn theme_discovery_diagnostics(
    config: &Config,
) -> Vec<crate::resource_resolver::ResourceDiagnostic> {
    discover_themes(config).diagnostics().to_vec()
}

pub(super) fn resolved_theme_resource(
    name: &str,
    config: &Config,
) -> anyhow::Result<(ResourceResolver, crate::resource_resolver::ResolvedResource)> {
    let file_name =
        theme_file_name(name).ok_or_else(|| anyhow::anyhow!("invalid theme name {name:?}"))?;
    let resource_name = file_name.strip_suffix(".toml").unwrap_or(&file_name);
    let resolver = ResourceResolver::new(config.workspace.clone(), config.workspace_trusted);
    let snapshot = resolver.discover(ResourceKind::Theme, &config.theme_paths);
    let resource = snapshot
        .get(resource_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("theme {name:?} was not discovered"))?;
    Ok((resolver, resource))
}

/// Resolve a theme by name through the shared global/project/explicit resolver.
#[cfg(test)]
pub fn theme_path(name: &str, config: &Config) -> Option<PathBuf> {
    resolved_theme_resource(name, config)
        .ok()
        .map(|(_, resource)| resource.path)
}

pub(super) fn read_theme_file_bounded(path: &Path) -> anyhow::Result<String> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("theme {} has no parent", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("theme {} has no file name", path.display()))?;
    // Reloads use the same no-follow, regular-file boundary as initial shared
    // resource reads. A trusted theme cannot be swapped for a symlink or FIFO
    // between discovery and `/theme reload`.
    let opened_path = parent.canonicalize()?.join(name);
    let bytes =
        octet_agent::secure_fs::read_regular_file_bounded(&opened_path, MAX_THEME_BYTES as usize)?;
    String::from_utf8(bytes)
        .map_err(|error| anyhow::anyhow!("theme {} is not UTF-8: {error}", path.display()))
}

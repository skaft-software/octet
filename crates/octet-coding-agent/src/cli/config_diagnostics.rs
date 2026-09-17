//! Bounded configuration-layer loading and compatibility diagnostics.
//! Layer selection, merging, environment handling, and persistence stay in `cli`.

use std::fmt;
use std::path::{Path, PathBuf};

use super::ConfigLayer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfigSourceKind {
    Global,
    Project,
}

impl fmt::Display for ConfigSourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "global",
            Self::Project => "project",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ConfigDiagnostic {
    source_kind: ConfigSourceKind,
    path: PathBuf,
    key: String,
    line: usize,
    column: usize,
    suggestion: Option<&'static str>,
}

impl fmt::Display for ConfigDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} config {}:{}:{}: unknown configuration key {:?}",
            self.source_kind,
            self.path.display(),
            self.line,
            self.column,
            self.key
        )?;
        if let Some(suggestion) = self.suggestion {
            write!(formatter, "; did you mean {suggestion:?}?")?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub(super) struct LoadedConfigLayer {
    pub(super) values: ConfigLayer,
    pub(super) diagnostics: Vec<ConfigDiagnostic>,
}

const CONFIG_KEYS: &[&str] = &[
    "model",
    "reasoning",
    "effect_policy",
    "reasoning_mode",
    "cache_retention",
    "theme",
    "color",
    "mouse",
    "plain",
    "show_images",
    "models",
    "allow_external_paths",
    "allow_edit",
    "allow_write",
    "allow_process",
    "allow_shell",
    "allow_remote_read",
    "shell_path",
    "bash_timeout_secs",
    "exec_timeout_secs",
    "max_output_bytes",
    "session_dir",
    "max_turns",
    "max_cost_microdollars",
    "cost_warning_microdollars",
    "context_files",
    "offline",
    "strict_config",
    "reload",
    "reload_poll_ms",
    "reload_debounce_ms",
    "reload_max_files",
    "reload_host",
    "telemetry",
    "enabled_extensions",
    "trusted_extensions",
    "system_prompt",
    "compaction",
];

/// Legacy configuration keys accepted and ignored for backward
/// compatibility. Removed settings must be listed here so older configs keep
/// loading without unknown-key warnings or strict-mode rejections.
const IGNORED_CONFIG_KEYS: &[&str] = &["show_turn_cost"];

const COMPACTION_KEYS: &[&str] = &[
    "mode",
    "policy",
    "enabled",
    "threshold_fraction",
    "max_active_tokens",
    "keep_recent_tokens",
    "keep_recent_turns",
    "compact_model",
];

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.chars().count()).collect::<Vec<_>>();
    let mut current = vec![0; previous.len()];
    for (left_index, left_character) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_character) in right.chars().enumerate() {
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + usize::from(left_character != right_character));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.chars().count()]
}

fn config_key_suggestion(key: &str) -> Option<&'static str> {
    let (prefix, leaf, candidates) = match key.rsplit_once('.') {
        Some(("compaction", leaf)) => ("compaction.", leaf, COMPACTION_KEYS),
        Some(_) => return None,
        None => ("", key, CONFIG_KEYS),
    };
    let (candidate, distance) = candidates
        .iter()
        .map(|candidate| (*candidate, edit_distance(leaf, candidate)))
        .min_by_key(|(_, distance)| *distance)?;
    let threshold = 2.max(leaf.chars().count() / 3);
    (distance <= threshold).then(|| {
        if prefix.is_empty() {
            candidate
        } else {
            match candidate {
                "mode" => "compaction.mode",
                "policy" => "compaction.policy",
                "enabled" => "compaction.enabled",
                "threshold_fraction" => "compaction.threshold_fraction",
                "max_active_tokens" => "compaction.max_active_tokens",
                "keep_recent_tokens" => "compaction.keep_recent_tokens",
                "keep_recent_turns" => "compaction.keep_recent_turns",
                "compact_model" => "compaction.compact_model",
                _ => unreachable!("compaction suggestion came from the fixed schema"),
            }
        }
    })
}

fn table_key_offset(table: &toml_edit::Table, segments: &[&str]) -> Option<usize> {
    let (segment, remaining) = segments.split_first()?;
    let key = table.key(segment)?;
    if remaining.is_empty() {
        return key.span().map(|span| span.start);
    }
    let item = table.get(segment)?;
    if let Some(table) = item.as_table() {
        table_key_offset(table, remaining)
    } else {
        inline_table_key_offset(item.as_inline_table()?, remaining)
    }
}

fn inline_table_key_offset(table: &toml_edit::InlineTable, segments: &[&str]) -> Option<usize> {
    let (segment, remaining) = segments.split_first()?;
    let key = table.key(segment)?;
    if remaining.is_empty() {
        return key.span().map(|span| span.start);
    }
    inline_table_key_offset(table.get(segment)?.as_inline_table()?, remaining)
}

fn ignored_config_path(path: &serde_ignored::Path<'_>, segments: &mut Vec<String>) {
    match path {
        serde_ignored::Path::Root => {}
        serde_ignored::Path::Map { parent, key } => {
            ignored_config_path(parent, segments);
            segments.push(key.clone());
        }
        serde_ignored::Path::Seq { parent, index } => {
            ignored_config_path(parent, segments);
            segments.push(index.to_string());
        }
        serde_ignored::Path::Some { parent }
        | serde_ignored::Path::NewtypeStruct { parent }
        | serde_ignored::Path::NewtypeVariant { parent } => {
            ignored_config_path(parent, segments);
        }
    }
}

fn config_key_location(source: &str, segments: &[String]) -> (usize, usize) {
    let offset = toml_edit::ImDocument::parse(source.to_owned())
        .ok()
        .and_then(|document| {
            let segments = segments.iter().map(String::as_str).collect::<Vec<_>>();
            table_key_offset(document.as_table(), &segments)
        })
        .unwrap_or(0);
    let prefix = &source[..offset.min(source.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .chars()
        .count()
        + 1;
    (line, column)
}

pub(super) fn report_config_diagnostics(
    diagnostics: &[ConfigDiagnostic],
    strict: bool,
) -> anyhow::Result<()> {
    if diagnostics.is_empty() {
        return Ok(());
    }
    if strict {
        let details = diagnostics
            .iter()
            .map(|diagnostic| format!("  - {diagnostic}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("strict configuration rejected unknown keys:\n{details}");
    }
    for diagnostic in diagnostics {
        crate::output::stderr_line(format!("warning: {diagnostic}"));
    }
    Ok(())
}

pub(super) fn read_layer(
    path: &Path,
    source_kind: ConfigSourceKind,
) -> anyhow::Result<LoadedConfigLayer> {
    const MAX_CONFIG_BYTES: usize = 1024 * 1024;
    let Some(name) = path.file_name() else {
        anyhow::bail!("config path {} has no file name", path.display());
    };
    let Some(parent) = path.parent() else {
        anyhow::bail!("config path {} has no parent", path.display());
    };
    let parent = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedConfigLayer::default())
        }
        Err(error) => return Err(error.into()),
    };
    let source = match octet_agent::secure_fs::read_regular_file_bounded(
        &parent.join(name),
        MAX_CONFIG_BYTES,
    ) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("config {} is not valid UTF-8", path.display()))?,
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(LoadedConfigLayer::default())
        }
        Err(error) => anyhow::bail!("cannot read config {}: {error}", path.display()),
    };

    let mut unknown_keys = Vec::new();
    let deserializer = toml::Deserializer::new(&source);
    let values = serde_ignored::deserialize(deserializer, |path| {
        let mut segments = Vec::new();
        ignored_config_path(&path, &mut segments);
        unknown_keys.push(segments);
    })
    .map_err(|error| anyhow::anyhow!("invalid config {}: {error}", path.display()))?;
    unknown_keys.retain(|segments| {
        segments.len() != 1 || !IGNORED_CONFIG_KEYS.contains(&segments[0].as_str())
    });
    unknown_keys.sort();
    unknown_keys.dedup();
    let diagnostics = unknown_keys
        .into_iter()
        .map(|segments| {
            let (line, column) = config_key_location(&source, &segments);
            let key = segments.join(".");
            ConfigDiagnostic {
                source_kind,
                path: path.to_path_buf(),
                suggestion: config_key_suggestion(&key),
                key,
                line,
                column,
            }
        })
        .collect();
    Ok(LoadedConfigLayer {
        values,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{build_config_with_global_path, Cli};
    use clap::Parser;

    #[test]
    fn unknown_config_keys_report_source_location_and_suggestion() {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("global.toml");
        std::fs::write(
            &global,
            "model = 'known'\nmodle = 'ignored'\n[compaction]\nkeep_recent_turn = 2\n",
        )
        .unwrap();

        let loaded = read_layer(&global, ConfigSourceKind::Global).unwrap();

        assert_eq!(loaded.values.model.as_deref(), Some("known"));
        assert_eq!(loaded.diagnostics.len(), 2);
        assert_eq!(loaded.diagnostics[0].key, "compaction.keep_recent_turn");
        assert_eq!(loaded.diagnostics[0].line, 4);
        assert_eq!(loaded.diagnostics[0].column, 1);
        assert_eq!(
            loaded.diagnostics[0].suggestion,
            Some("compaction.keep_recent_turns")
        );
        assert_eq!(loaded.diagnostics[1].key, "modle");
        assert_eq!(loaded.diagnostics[1].line, 2);
        assert_eq!(loaded.diagnostics[1].column, 1);
        assert_eq!(loaded.diagnostics[1].suggestion, Some("model"));
        assert_eq!(loaded.diagnostics[1].source_kind, ConfigSourceKind::Global);
        assert_eq!(loaded.diagnostics[1].path, global);
    }

    #[test]
    fn unknown_config_keys_warn_by_default_and_fail_in_cli_strict_mode() {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "modle = 'ignored'\n").unwrap();
        let mut cli = Cli::try_parse_from(["octet"]).unwrap();
        cli.workspace = Some(directory.path().into());
        assert!(build_config_with_global_path(cli, directory.path(), Some(&global)).is_ok());

        let mut cli = Cli::try_parse_from(["octet"]).unwrap();
        cli.workspace = Some(directory.path().into());
        cli.strict_config = true;
        let error = build_config_with_global_path(cli, directory.path(), Some(&global))
            .unwrap_err()
            .to_string();
        assert!(error.contains("strict configuration rejected unknown keys"));
        assert!(error.contains("global config"));
        assert!(error.contains(&format!("{}:1:1", global.display())));
        assert!(error.contains("unknown configuration key \"modle\""));
        assert!(error.contains("did you mean \"model\"?"));
    }

    #[test]
    fn project_config_can_opt_into_strict_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        let project = directory.path().join(".octet/config.toml");
        std::fs::write(&project, "strict_config = true\nthemee = 'ignored'\n").unwrap();
        let mut cli = Cli::try_parse_from(["octet"]).unwrap();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;

        let error = build_config_with_global_path(
            cli,
            directory.path(),
            Some(&directory.path().join("missing-global.toml")),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("project config"));
        assert!(error.contains(&format!("{}:2:1", project.display())));
        assert!(error.contains("themee"));
    }

    #[test]
    fn accepted_config_aliases_do_not_emit_unknown_key_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("global.toml");
        std::fs::write(
            &global,
            "exec_timeout_secs = 30\n[compaction]\npolicy = 'local'\n",
        )
        .unwrap();

        let loaded = read_layer(&global, ConfigSourceKind::Global).unwrap();

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.values.bash_timeout_secs, Some(30));
        assert_eq!(
            loaded.values.compaction.unwrap().mode.as_deref(),
            Some("local")
        );
    }

    #[test]
    fn missing_file_and_missing_parent_load_empty_layers() {
        let directory = tempfile::tempdir().unwrap();
        for path in [
            directory.path().join("missing.toml"),
            directory.path().join("missing-parent/config.toml"),
        ] {
            let loaded = read_layer(&path, ConfigSourceKind::Global).unwrap();
            assert!(loaded.values.model.is_none());
            assert!(loaded.diagnostics.is_empty());
            assert!(report_config_diagnostics(&loaded.diagnostics, true).is_ok());
        }
    }

    #[test]
    fn malformed_toml_invalid_values_and_invalid_utf8_remain_fatal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for source in ["model = [", "max_turns = 'not a number'"] {
            std::fs::write(&path, source).unwrap();
            let error = read_layer(&path, ConfigSourceKind::Global)
                .unwrap_err()
                .to_string();
            assert!(error.starts_with(&format!("invalid config {}:", path.display())));
        }
        std::fs::write(&path, [0xff]).unwrap();
        let error = read_layer(&path, ConfigSourceKind::Global)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!("config {} is not valid UTF-8", path.display())
        );
    }

    #[test]
    fn reads_are_bounded_at_one_mebibyte_and_require_regular_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, vec![b' '; 1024 * 1024]).unwrap();
        assert!(read_layer(&path, ConfigSourceKind::Global).is_ok());
        std::fs::write(&path, vec![b' '; 1024 * 1024 + 1]).unwrap();
        let error = read_layer(&path, ConfigSourceKind::Global)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with(&format!("cannot read config {}:", path.display())));
        assert!(read_layer(directory.path(), ConfigSourceKind::Global).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_files_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.toml");
        let link = directory.path().join("config.toml");
        std::fs::write(&target, "model = 'known'\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_layer(&link, ConfigSourceKind::Global).is_err());
    }

    #[test]
    fn inline_table_locations_and_ignored_legacy_keys_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project.toml");
        std::fs::write(
            &path,
            "show_turn_cost = true\ncompaction = { mode = 'local', keep_recent_turn = 2 }\n",
        )
        .unwrap();
        let loaded = read_layer(&path, ConfigSourceKind::Project).unwrap();
        assert_eq!(loaded.diagnostics.len(), 1);
        let diagnostic = &loaded.diagnostics[0];
        assert_eq!(diagnostic.source_kind, ConfigSourceKind::Project);
        assert_eq!(diagnostic.key, "compaction.keep_recent_turn");
        assert_eq!((diagnostic.line, diagnostic.column), (2, 32));
        assert_eq!(diagnostic.suggestion, Some("compaction.keep_recent_turns"));
        assert_eq!(config_key_suggestion("unrelated.nested.typo"), None);
        assert_eq!(config_key_suggestion("zzzzzzzzzzzzzzzzzzzz"), None);
    }
}

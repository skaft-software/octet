//! `octet migrate pi`: a short, guided way to turn a Pi setup on in octet.
//!
//! The scan never runs package code. Turning the setup on only installs the
//! official Pi adapter bundle and enables it in octet's user config (backed up
//! first). Pi's own directory is read, never written; `--undo` turns it off.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use super::{MigrationPath, MigrationReport};
use crate::output::{StyledLine, TextStyle::*};

pub(super) const ADAPTER: &str = "octet-pi-compat";
const MINIMUM_NODE: (u64, u64) = (22, 19);

/// What the guided command was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// Show the summary and ask before changing anything.
    Ask,
    /// Show the summary and change nothing.
    Preview,
    /// Turn the setup on without asking.
    Apply,
    /// Turn the adapter off again.
    Undo,
}

/// Plain counts a newcomer can read at a glance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Overview {
    pub(super) extensions: usize,
    pub(super) active_packages: usize,
    pub(super) skills: usize,
    pub(super) prompts: usize,
    pub(super) themes: usize,
    pub(super) off_packages: usize,
    pub(super) blocked: usize,
    pub(super) warnings: usize,
}

impl Overview {
    pub(super) fn from_report(report: &MigrationReport) -> Self {
        let off_packages = report
            .packages
            .iter()
            .filter(|package| {
                !package.resources.is_empty()
                    && package.resources.iter().all(|resource| !resource.enabled)
            })
            .count();
        Self {
            extensions: report.found.extensions,
            active_packages: report.packages.len() - off_packages,
            skills: report.found.skills,
            prompts: report.found.prompts,
            themes: report.found.themes,
            off_packages,
            blocked: report.migration.get(MigrationPath::Blocked),
            warnings: report.diagnostics.len()
                + report
                    .packages
                    .iter()
                    .map(|package| package.diagnostics.len())
                    .sum::<usize>(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.extensions + self.skills + self.prompts + self.themes + self.active_packages == 0
    }
}

struct Style {
    color: bool,
}

impl Style {
    fn detect(mode: crate::config::ColorMode) -> Self {
        Self {
            color: std::io::stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none()
                && mode != crate::config::ColorMode::Never,
        }
    }

    fn print(&self, line: StyledLine) {
        crate::output::stdout_styled_line(&line, self.color);
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn home_relative(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// The overview block, as lines. Pure so it can be tested byte for byte.
fn overview_lines(pi_home: &Path, overview: &Overview) -> Vec<StyledLine> {
    let mut lines = vec![StyledLine::default()
        .text(Bold, "Your Pi setup")
        .text(Plain, "  ")
        .text(Dim, home_relative(pi_home))];
    lines.push(StyledLine::default());
    if overview.is_empty() {
        lines.push(StyledLine::default().text(
            Plain,
            "  Nothing to bring over: no enabled extensions, skills, prompts or themes.",
        ));
        return lines;
    }
    let mut row = |label: &str, value: String, note: &str| {
        lines.push(
            StyledLine::default()
                .text(Plain, format!("  {label:<12}{value:<22}"))
                .text(Dim, note),
        );
    };
    if overview.extensions > 0 {
        row(
            "Extensions",
            format!(
                "{} {}",
                overview.extensions,
                if overview.active_packages > 0 {
                    format!(
                        "from {}",
                        plural(overview.active_packages, "package", "packages")
                    )
                } else {
                    String::new()
                }
            ),
            "run in octet through the Pi adapter",
        );
    }
    if overview.skills > 0 {
        row(
            "Skills",
            overview.skills.to_string(),
            "work as they do in Pi",
        );
    }
    if overview.prompts > 0 {
        row(
            "Prompts",
            overview.prompts.to_string(),
            "work as they do in Pi",
        );
    }
    if overview.themes > 0 {
        row(
            "Themes",
            overview.themes.to_string(),
            "work as they do in Pi",
        );
    }
    if overview.off_packages > 0 {
        row(
            "Turned off",
            plural(overview.off_packages, "package", "packages"),
            "stay off, as in Pi",
        );
    }
    if overview.blocked > 0 || overview.warnings > 0 {
        lines.push(StyledLine::default());
        if overview.blocked > 0 {
            lines.push(
                StyledLine::default()
                    .text(Plain, "  ")
                    .text(Warning, "!")
                    .text(
                        Plain,
                        format!(
                            " {} can't run in octet yet",
                            plural(overview.blocked, "item", "items")
                        ),
                    ),
            );
        }
        if overview.warnings > 0 {
            lines.push(
                StyledLine::default()
                    .text(Plain, "  ")
                    .text(Warning, "!")
                    .text(
                        Plain,
                        format!(
                            " {} could not be fully checked",
                            plural(overview.warnings, "file", "files")
                        ),
                    ),
            );
        }
        lines.push(
            StyledLine::default()
                .text(Plain, "    ")
                .text(Dim, "See which: octet migrate pi --details"),
        );
    }
    lines
}

fn what_changes_lines() -> Vec<StyledLine> {
    vec![
        StyledLine::default(),
        StyledLine::default().text(Bold, "What turning it on does"),
        StyledLine::default().text(Plain, "  · Installs the Pi adapter and turns it on in octet (your octet config is backed up first)"),
        StyledLine::default().text(Plain, "  · Leaves your Pi settings as they are. Pi keeps working exactly as before"),
        StyledLine::default().text(Plain, "  · Runs your Pi extensions with your user permissions, as Pi does; they keep their data where Pi keeps it"),
        StyledLine::default(),
    ]
}

fn print(lines: Vec<StyledLine>, style: &Style) {
    for line in lines {
        style.print(line);
    }
}

/// Parsed `node --version`, when Node is on PATH.
fn node_version() -> Option<(u64, u64)> {
    let output = std::process::Command::new("node")
        .arg("--version")
        .output()
        .ok()?;
    parse_node_version(std::str::from_utf8(&output.stdout).ok()?)
}

pub(super) fn parse_node_version(text: &str) -> Option<(u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn adapter_enabled(config: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(config) else {
        return false;
    };
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    document
        .get("enabled_extensions")
        .and_then(|item| item.as_array())
        .is_some_and(|names| names.iter().any(|name| name.as_str() == Some(ADAPTER)))
}

/// Copy octet's user config aside before changing it. `None` when there is no
/// config yet (nothing to back up).
pub(super) fn backup_config(config: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !config.exists() {
        return Ok(None);
    }
    let directory = config
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent"))?
        .join("backups");
    std::fs::create_dir_all(&directory)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let mut target = directory.join(format!("config-before-pi-migration-{stamp}.toml"));
    let mut suffix = 1;
    while target.exists() {
        target = directory.join(format!("config-before-pi-migration-{stamp}-{suffix}.toml"));
        suffix += 1;
    }
    std::fs::copy(config, &target)?;
    Ok(Some(target))
}

fn confirm(question: &str) -> bool {
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "{question} [y/N] ");
    let _ = stdout.flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub(super) async fn run(
    action: Action,
    report: &MigrationReport,
    pi_home: &Path,
    color: crate::config::ColorMode,
) -> anyhow::Result<()> {
    let style = Style::detect(color);
    let config = crate::cli::global_config_path()
        .ok_or_else(|| anyhow::anyhow!("your home directory is unavailable"))?;

    if action == Action::Undo {
        if !adapter_enabled(&config) {
            crate::output::stdout_line("The Pi adapter is already off. Nothing changed.");
            return Ok(());
        }
        let backup = backup_config(&config)?;
        crate::cli::persist_extension_enabled_to_path(ADAPTER, false, &config)?;
        style.print(
            StyledLine::default()
                .text(Success, "✓")
                .text(Plain, " Pi adapter turned off."),
        );
        crate::output::stdout_line("  Your Pi setup and Pi itself were not touched.");
        if let Some(backup) = backup {
            style.print(StyledLine::default().text(
                Dim,
                format!("  Previous octet config: {}", home_relative(&backup)),
            ));
        }
        return Ok(());
    }

    let overview = Overview::from_report(report);
    print(overview_lines(pi_home, &overview), &style);
    if overview.is_empty() {
        return Ok(());
    }
    if adapter_enabled(&config) {
        crate::output::stdout_line("");
        style.print(
            StyledLine::default()
                .text(Success, "✓")
                .text(Plain, " Already on. Start octet as usual: ")
                .text(Bold, "octet"),
        );
        style.print(StyledLine::default().text(Dim, "  Turn it off: octet migrate pi --undo"));
        return Ok(());
    }
    print(what_changes_lines(), &style);

    let node = node_version();
    let node_ok = node.is_some_and(|version| version >= MINIMUM_NODE);
    if !node_ok {
        let found = node.map_or("not found".to_owned(), |(major, minor)| {
            format!("found {major}.{minor}")
        });
        style.print(StyledLine::default().text(Warning, "!").text(
            Plain,
            format!(
                " Pi extensions need Node.js {}.{} or newer ({found}).",
                MINIMUM_NODE.0, MINIMUM_NODE.1
            ),
        ));
        crate::output::stdout_line(
            "  Install it from https://nodejs.org, then run octet migrate pi again.",
        );
        return Ok(());
    }

    let proceed = match action {
        Action::Preview => {
            style.print(
                StyledLine::default()
                    .text(Plain, "Nothing was changed. To turn it on: ")
                    .text(Bold, "octet migrate pi"),
            );
            return Ok(());
        }
        Action::Apply => true,
        Action::Ask if std::io::stdin().is_terminal() => confirm("Turn on your Pi setup in octet?"),
        Action::Ask => {
            style.print(
                StyledLine::default()
                    .text(
                        Plain,
                        "Nothing was changed. To turn it on without a prompt: ",
                    )
                    .text(Bold, "octet migrate pi --yes"),
            );
            return Ok(());
        }
        Action::Undo => unreachable!("handled above"),
    };
    if !proceed {
        crate::output::stdout_line("Nothing was changed.");
        return Ok(());
    }

    let root = crate::extension_package::extensions_root()?;
    if crate::extension_bundle::ensure_installed(&root, ADAPTER).is_err() {
        style.print(StyledLine::default().text(Dim, "Installing the Pi adapter…"));
        crate::extension_bundle::install_official(&root, ADAPTER, false, &|_, _| {}).await?;
    }
    let backup = backup_config(&config)?;
    crate::cli::persist_extension_enabled_to_path(ADAPTER, true, &config)?;

    crate::output::stdout_line("");
    style.print(
        StyledLine::default()
            .text(Success, "✓")
            .text(Plain, " Done. Start octet as usual: ")
            .text(Bold, "octet"),
    );
    if let Some(backup) = backup {
        style.print(StyledLine::default().text(
            Dim,
            format!("  Previous octet config: {}", home_relative(&backup)),
        ));
    }
    style.print(StyledLine::default().text(Dim, "  Turn it off any time: octet migrate pi --undo"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overview() -> Overview {
        Overview {
            extensions: 28,
            active_packages: 13,
            skills: 85,
            prompts: 87,
            themes: 50,
            off_packages: 16,
            blocked: 0,
            warnings: 4,
        }
    }

    #[test]
    fn overview_is_short_plain_and_points_at_details() {
        let lines = overview_lines(Path::new("/nowhere/.pi/agent"), &overview())
            .iter()
            .map(|line| line.render(false, false))
            .collect::<Vec<_>>();
        let text = lines.join("\n");
        assert_eq!(
            text,
            "Your Pi setup  /nowhere/.pi/agent\n\
             \n  Extensions  28 from 13 packages   run in octet through the Pi adapter\
             \n  Skills      85                    work as they do in Pi\
             \n  Prompts     87                    work as they do in Pi\
             \n  Themes      50                    work as they do in Pi\
             \n  Turned off  16 packages           stay off, as in Pi\
             \n\
             \n  ! 4 files could not be fully checked\
             \n    See which: octet migrate pi --details"
        );
        assert!(lines.len() <= 12, "the overview must stay one screen");
        assert!(!text.contains("NATIVE PORT") && !text.contains("BRIDGE"));
    }

    #[test]
    fn empty_setup_says_so_in_one_line() {
        let lines = overview_lines(Path::new("/p"), &Overview::default())
            .iter()
            .map(|line| line.render(false, false))
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].contains("Nothing to bring over"));
    }

    #[test]
    fn color_is_only_added_when_requested() {
        let plain = overview_lines(Path::new("/p"), &overview())
            .iter()
            .map(|line| line.render(false, false))
            .collect::<Vec<_>>()
            .join("\n");
        let colored = overview_lines(Path::new("/p"), &overview())
            .iter()
            .map(|line| line.render(true, true))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!plain.contains('\x1b'));
        assert!(colored.contains("\x1b[1m"));
    }

    #[test]
    fn node_versions_parse_with_or_without_prefix() {
        assert_eq!(parse_node_version("v22.20.0\n"), Some((22, 20)));
        assert_eq!(parse_node_version("22.19.1"), Some((22, 19)));
        assert_eq!(parse_node_version("garbage"), None);
        assert!((22, 18) < MINIMUM_NODE && (22, 19) >= MINIMUM_NODE && (24, 0) >= MINIMUM_NODE);
    }

    #[test]
    fn config_backup_copies_and_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        assert_eq!(backup_config(&config).unwrap(), None);
        std::fs::write(&config, "model = \"a\"\n").unwrap();
        let first = backup_config(&config).unwrap().unwrap();
        let second = backup_config(&config).unwrap().unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(first).unwrap(), "model = \"a\"\n");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "model = \"a\"\n");
    }

    #[test]
    fn adapter_enabled_reads_only_the_activation_list() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        assert!(!adapter_enabled(&config));
        std::fs::write(&config, "trusted_extensions = [\"octet-pi-compat\"]\n").unwrap();
        assert!(!adapter_enabled(&config));
        std::fs::write(&config, "enabled_extensions = [\"octet-pi-compat\"]\n").unwrap();
        assert!(adapter_enabled(&config));
    }
}

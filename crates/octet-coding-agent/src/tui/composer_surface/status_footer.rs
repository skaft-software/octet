//! The two-line terminal footer. Facts are captured by the input owner; paint
//! never reads the workspace or walks the session ledger.

use std::path::{Path, PathBuf};

use octet_ai::{secure_fs::read_regular_file_bounded, Usage};
use sexy_tui_rs::{truncate_to_width, visible_width};

use super::{fit_line, footer_workspace, FooterControl, FooterSpan, ShellState};
use crate::tui::view::sanitize_ordinary_surface_cell;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FooterUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl FooterUsage {
    pub(crate) fn add(&mut self, usage: Usage) {
        self.input = self.input.saturating_add(usage.input_tokens);
        self.output = self.output.saturating_add(usage.output_tokens);
        self.cache_read = self.cache_read.saturating_add(usage.cache_read_tokens);
        self.cache_write = self.cache_write.saturating_add(usage.cache_write_tokens);
    }
}

pub(crate) fn workspace_branch(workspace: &Path) -> Option<String> {
    // Workspace resolution ordinarily ends at .git. Explicit workspace paths
    // can be nested; stop before searching an ancestor HOME dotfiles repo.
    let home = dirs::home_dir();
    let dot_git = workspace
        .ancestors()
        .take_while(|path| *path == workspace || Some(*path) != home.as_deref())
        .map(|path| path.join(".git"))
        .find(|path| path.symlink_metadata().is_ok())?;
    let git_dir = if dot_git.symlink_metadata().ok()?.is_dir() {
        dot_git
    } else {
        let pointer = String::from_utf8(read_regular_file_bounded(&dot_git, 4096).ok()?).ok()?;
        let target = PathBuf::from(pointer.trim().strip_prefix("gitdir: ")?);
        dot_git.parent()?.join(target).canonicalize().ok()?
    };
    let head =
        String::from_utf8(read_regular_file_bounded(&git_dir.join("HEAD"), 4096).ok()?).ok()?;
    let branch = head.trim_end().strip_prefix("ref: refs/heads/")?;
    (!branch.is_empty() && !branch.chars().any(char::is_control)).then(|| branch.to_owned())
}

fn tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1_000.0),
        10_000..=999_999 => format!("{:.0}k", count as f64 / 1_000.0),
        1_000_000..=9_999_999 => format!("{:.1}M", count as f64 / 1_000_000.0),
        _ => format!("{:.0}M", count as f64 / 1_000_000.0),
    }
}

pub(super) fn show_workspace(state: &ShellState, width: u16) -> bool {
    let layout = state.theme.layout_for_width(width);
    width >= 12 && state.size.1 >= 7 && layout.show_footer && !layout.show_header
}

pub(super) fn render(state: &ShellState, width: u16) -> (Vec<String>, Vec<FooterSpan>) {
    let theme = &state.theme;
    let layout = theme.layout_for_width(width);
    let identity = layout.show_footer && !layout.show_header;
    let unicode = theme.unicode();
    let separator = if unicode { " • " } else { " - " };
    let ellipsis = if unicode { "…" } else { "..." };
    let inset = usize::from(layout.composer_padding).min(usize::from(width.saturating_sub(1)) / 2);
    let available = usize::from(width) - 2 * inset;
    let padding = " ".repeat(inset);
    let cell = |text: &str| sanitize_ordinary_surface_cell(text, unicode);
    let mut rows = Vec::with_capacity(2);
    if show_workspace(state, width) {
        let path = state.workspace.as_deref().map_or_else(String::new, |path| {
            footer_workspace(path, super::footer_home(), available, unicode)
        });
        let mut line = path;
        if let Some(branch) = &state.workspace_branch {
            line.push_str(&format!(" ({})", cell(branch)));
        }
        if let Some(name) = &state.session_name {
            line.push_str(separator);
            line.push_str(&cell(name));
        }
        rows.push(fit_line(
            &format!(
                "{padding}{}",
                theme.fg("dim", &truncate_to_width(&line, available, Some(ellipsis)))
            ),
            width,
        ));
    }
    let active = state.run.is_active();
    let mut parts = Vec::new();
    let mut spans = Vec::new();
    let mut context_width = None;
    if layout.show_status_line {
        let usage = state.footer_usage;
        let tags = if unicode {
            ["↑", "↓", "R", "W"]
        } else {
            ["In", "Out", "R", "W"]
        };
        for (tag, n) in tags.into_iter().zip([
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write,
        ]) {
            if n != 0 {
                parts.push(theme.fg("dim", &format!("{tag}{}", tokens(n))));
            }
        }
        if let Some(rate) = state.cache_hit_rate_basis_points {
            parts.push(theme.fg("dim", &format!("CH{:.1}%", f64::from(rate) / 100.0)));
        }
        if let Some(cost) = state.displayed_session_cost_microdollars() {
            let text = format!(
                "${:.3}{}",
                cost as f64 / 1_000_000.0,
                state.session_cost_suffix(cost)
            );
            let budget = state
                .max_session_cost_microdollars
                .filter(|limit| *limit > 0);
            parts.push(match budget.map(|limit| cost as f64 / limit as f64) {
                Some(ratio) if ratio >= 0.9 => theme.fg("error", &text),
                Some(ratio) if ratio >= 0.5 => theme.fg("warning", &text),
                _ => theme.fg("dim", &text),
            });
        }
        let context = if active {
            state.run_context_estimate
        } else {
            state.context_estimate
        };
        if let Some((used, limit)) = context.filter(|&(_, limit)| limit != 0) {
            let used = used.saturating_add(if active {
                state.live_generated_tokens().unwrap_or(0)
            } else {
                0
            });
            let percent = used as f64 * 100.0 / limit as f64;
            let approx = if active && state.turn_generation_started_at.is_some() {
                "~"
            } else {
                ""
            };
            let auto = if state.footer_auto_compact {
                " (auto)"
            } else {
                ""
            };
            let text = format!("{approx}{percent:.1}%/{}{auto}", tokens(limit));
            context_width = Some(visible_width(&text));
            parts.push(if percent > 90.0 {
                theme.fg("error", &text)
            } else if percent > 70.0 {
                theme.fg("warning", &text)
            } else {
                theme.fg("dim", &text)
            });
        }
    }
    let left = parts.join(" ");
    let left_width = visible_width(&left);
    if let Some(context_width) = context_width.filter(|_| left_width <= available) {
        spans.push(FooterSpan::new(
            FooterControl::Context,
            inset + left_width - context_width,
            inset + left_width,
        ));
    }
    let left = truncate_to_width(&left, available, Some(ellipsis));
    let remaining =
        available.saturating_sub(visible_width(&left) + if left.is_empty() { 0 } else { 2 });
    let right = if identity {
        let model = if active {
            state
                .run_model_display
                .as_deref()
                .or(state.run_model.as_deref())
        } else {
            None
        };
        let model = model.unwrap_or(if state.model_display.is_empty() {
            &state.model
        } else {
            &state.model_display
        });
        let effort = if active {
            state.run_reasoning.as_deref().unwrap_or(&state.reasoning)
        } else {
            &state.reasoning
        };
        let mut full = cell(if model.is_empty() { "no-model" } else { model });
        let effort = cell(if effort == "off" {
            "thinking off"
        } else {
            effort
        });
        if !effort.is_empty() {
            full.push_str(separator);
            full.push_str(&effort);
        }
        let provider = if active {
            state.run_provider.as_deref().unwrap_or(&state.provider)
        } else {
            &state.provider
        };
        if state.model_labels.provider_count() > 1 && !provider.is_empty() {
            let candidate = format!("({}) {full}", cell(provider));
            if visible_width(&candidate) <= remaining {
                full = candidate;
            }
        }
        let full_width = visible_width(&full);
        if full_width <= remaining {
            let end = inset + available;
            let model_end = if effort.is_empty() {
                end
            } else {
                let effort_start = end - visible_width(&effort);
                spans.push(FooterSpan::new(FooterControl::Thinking, effort_start, end));
                effort_start - visible_width(separator)
            };
            spans.push(FooterSpan::new(
                FooterControl::Model,
                end - full_width,
                model_end,
            ));
            theme.fg("dim", &full)
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let gap = if right.is_empty() {
        0
    } else {
        available - visible_width(&left) - visible_width(&right)
    };
    rows.push(fit_line(
        &format!("{padding}{left}{}{right}", " ".repeat(gap)),
        width,
    ));
    (rows, spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::test_theme_from_source;
    use sexy_tui_rs::strip_terminal_sequences;

    #[test]
    fn two_line_footer_uses_real_facts_and_fits_narrow_panes() {
        let mut state = ShellState::with_theme(test_theme_from_source(
            "[layout]\nstatus_footer = 'two_line'",
        ));
        state.size = (100, 24);
        state.workspace = Some(PathBuf::from("/projects/fixture"));
        state.workspace_branch = Some("main".into());
        state.session_name = Some("demo".into());
        state.model = "fixture-model".into();
        state.reasoning = "medium".into();
        state.context_estimate = Some((23_500, 500_000));
        state.footer_auto_compact = true;
        state.cache_hit_rate_basis_points = Some(7_470);
        state.session_cost_microdollars = Some(210_000);
        state.footer_usage.add(Usage {
            input_tokens: 90_000,
            output_tokens: 1_800,
            cache_read_tokens: 124_000,
            ..Usage::default()
        });
        let (rows, spans) = render(&state, 100);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            strip_terminal_sequences(&rows[0]).trim(),
            "/projects/fixture (main) • demo"
        );
        let stats = strip_terminal_sequences(&rows[1]);
        assert!(
            stats.starts_with(" ↑90k ↓1.8k R124k CH74.7% $0.210 4.7%/500k (auto)"),
            "{stats}"
        );
        assert!(
            stats.trim_end().ends_with("fixture-model • medium"),
            "{stats}"
        );
        for (control, label) in [
            (FooterControl::Context, "4.7%/500k (auto)"),
            (FooterControl::Model, "fixture-model"),
            (FooterControl::Thinking, "medium"),
        ] {
            let start = visible_width(&stats[..stats.find(label).unwrap()]);
            let span = spans.iter().find(|span| span.control == control).unwrap();
            assert_eq!(span.columns, start..start + visible_width(label));
        }
        for width in 1..=100 {
            let (rows, spans) = render(&state, width);
            assert!(spans
                .iter()
                .all(|span| span.columns.end <= usize::from(width)));
            assert!(rows
                .iter()
                .all(|row| visible_width(row) <= usize::from(width)));
            assert_eq!(rows.len(), if width < 12 { 1 } else { 2 });
        }
        state.size.1 = 5;
        assert_eq!(render(&state, 100).0.len(), 1);
    }

    #[test]
    fn branch_reads_regular_git_head_and_linked_worktrees_without_running_git() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let git = root.join(".git");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(workspace_branch(&root).as_deref(), Some("main"));
        let worktree = root.join("checkout");
        let metadata = git.join("worktrees/checkout");
        std::fs::create_dir_all(&metadata).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            "gitdir: ../.git/worktrees/checkout\n",
        )
        .unwrap();
        std::fs::write(metadata.join("HEAD"), "ref: refs/heads/topic\n").unwrap();
        assert_eq!(workspace_branch(&worktree).as_deref(), Some("topic"));
        std::fs::write(metadata.join("HEAD"), "12345\n").unwrap();
        assert_eq!(workspace_branch(&worktree), None);
    }
}

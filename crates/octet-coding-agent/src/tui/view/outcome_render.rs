//! Terminal presentation for completed, failed, and interrupted runs.

use std::time::Duration;

use crate::presentation::{format_duration, RunOutcome};
use crate::tui::theme::OctetTheme;

use super::terminal_text::sanitize_for_terminal;
use super::{
    finish_transcript_block, fit_line, semantic_separator, subdued_text, wrap_hanging, OutcomeBlock,
};

const MAX_OUTCOME_DETAIL_BYTES: usize = 4 * 1024;

/// Subdued detail under a completed turn whose delegated workers are still
/// alive. The wording lives here rather than in `RunOutcome` because liveness
/// is a render-time fact: the same settled outcome block stops showing the line
/// the moment the roster settles.
pub(super) const SUBAGENTS_RUNNING_DETAIL: &str =
    "subagents are running; inspect them in the /subagents menu.";

pub(super) fn completion_text(
    elapsed: Duration,
    separator: &str,
    tokens_per_second: Option<f64>,
) -> String {
    completion_status_text("completed", elapsed, separator, tokens_per_second)
}

fn completion_status_text(
    status: &str,
    elapsed: Duration,
    separator: &str,
    tokens_per_second: Option<f64>,
) -> String {
    let mut text = format!("{status}{separator}{}", format_duration(elapsed));
    if let Some(rate) = tokens_per_second.filter(|rate| rate.is_finite() && *rate > 0.0) {
        text.push_str(&format!("{separator}{rate:.0} tok/s"));
    }
    text
}

fn outcome_line(
    outcome: &RunOutcome,
    tokens_per_second: Option<f64>,
    theme: &OctetTheme,
) -> String {
    let separator = semantic_separator(theme);
    match outcome {
        // `CompletedWithWarnings` deliberately renders exactly like
        // `Completed`: the same success glyph, the same `completed · duration ·
        // rate tok/s` text, the same success-led intro. Per-call tool failures
        // are already rendered by their own tool blocks above this line, and the
        // warning count stays in the model for exit status and telemetry. It is
        // never transcript wording, so the two variants cannot drift apart.
        RunOutcome::Completed { elapsed, .. }
        | RunOutcome::CompletedWithWarnings { elapsed, .. } => {
            let text = subdued_text(
                theme,
                &completion_text(*elapsed, separator, tokens_per_second),
            );
            format!("{} {text}", theme.fg("success", theme.glyph("success")))
        }
        RunOutcome::Failed { elapsed, .. } => format!(
            "{} {}",
            theme.fg("error", theme.glyph("error")),
            theme.fg(
                "error",
                &format!("failed{separator}{}", format_duration(*elapsed))
            )
        ),
        RunOutcome::Interrupted { elapsed } => format!(
            "{} {}",
            theme.fg("warning", theme.glyph("interrupt")),
            subdued_text(
                theme,
                &format!("interrupted{separator}{}", format_duration(*elapsed)),
            )
        ),
        RunOutcome::NeedsInput { .. } => format!(
            "{} {}",
            theme.fg("warning", theme.glyph("note")),
            subdued_text(theme, "needs input")
        ),
    }
}

pub(super) fn bounded_outcome_detail(raw: &str) -> String {
    let mut safe = sanitize_for_terminal(raw);
    if safe.len() <= MAX_OUTCOME_DETAIL_BYTES {
        return safe;
    }

    let mut end = MAX_OUTCOME_DETAIL_BYTES - '…'.len_utf8();
    while end > 0 && !safe.is_char_boundary(end) {
        end -= 1;
    }
    safe.truncate(end);
    safe.push('…');
    safe
}

pub(super) fn render_outcome(
    outcome: &OutcomeBlock,
    theme: &OctetTheme,
    width: u16,
    subagents_running: bool,
) -> Vec<String> {
    let mut lines = vec![fit_line(
        &outcome_line(&outcome.outcome, outcome.tokens_per_second, theme),
        width,
    )];
    let detail = match &outcome.outcome {
        // Inference diagnostics are credential-redacted at the request boundary.
        // Bound and terminal-sanitize them again at this presentation boundary.
        RunOutcome::Failed { reason, .. } => Some(("error", reason.clone())),
        RunOutcome::NeedsInput { prompt } => Some(("warning", prompt.clone())),
        // A completed turn carries no warning detail.
        _ => None,
    };
    if let Some((role, detail)) = detail {
        let safe = bounded_outcome_detail(&detail);
        for source_line in safe.split('\n') {
            if source_line.is_empty() {
                lines.push(String::new());
                continue;
            }
            lines.extend(wrap_hanging(
                &theme.fg(role, source_line),
                "  ",
                "  ",
                width,
            ));
        }
    }
    // A completed turn whose delegated workers are still alive says so in one
    // subdued, informational line - never a warning and never a substitute for
    // the delegated event's own transcript block. The flag is derived from the
    // live roster at render time, so the line vanishes when the workers settle.
    let completed = matches!(
        &outcome.outcome,
        RunOutcome::Completed { .. } | RunOutcome::CompletedWithWarnings { .. }
    );
    if completed && subagents_running {
        lines.extend(wrap_hanging(
            &subdued_text(theme, SUBAGENTS_RUNNING_DETAIL),
            "  ",
            "  ",
            width,
        ));
    }
    finish_transcript_block(lines)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sexy_tui_rs::strip_terminal_sequences;

    use super::super::{block_copy_text, OutcomeBlock, TranscriptBlock};
    use super::*;
    use crate::presentation::RunSummary;

    #[test]
    fn completed_variants_render_byte_identically_and_never_warn() {
        let theme = crate::tui::theme::test_theme();
        let summary = RunSummary {
            files_changed: 2,
            tool_calls: 4,
            warnings: 0,
        };
        let completed = RunOutcome::Completed {
            elapsed: Duration::from_secs(60 + 23),
            summary: summary.clone(),
        };
        let with_warnings = RunOutcome::CompletedWithWarnings {
            elapsed: Duration::from_secs(60 + 23),
            warnings: 13,
            summary: RunSummary {
                files_changed: 1,
                tool_calls: 9,
                warnings: 13,
            },
        };

        for rate in [None, Some(216.0)] {
            assert_eq!(
                outcome_line(&completed, rate, &theme),
                outcome_line(&with_warnings, rate, &theme),
                "the two completed variants must render byte-identically"
            );
        }
        let line = strip_terminal_sequences(&outcome_line(&with_warnings, Some(216.0), &theme));
        assert_eq!(line, "✓ completed · 1m23s · 216 tok/s");
        assert!(!line.to_ascii_lowercase().contains("warning"), "{line}");

        // The success glyph and role are the ones the completed line uses; the
        // warning glyph and role never reach it.
        let styled = outcome_line(&with_warnings, Some(216.0), &theme);
        assert!(
            styled.contains(&theme.fg("success", theme.glyph("success"))),
            "{styled:?}"
        );
        assert!(
            !styled.contains(&theme.fg("warning", theme.glyph("warning"))),
            "{styled:?}"
        );

        // The whole block is identical too, and no line carries warning wording.
        let block = |outcome: RunOutcome| OutcomeBlock::new(outcome, Some(216.0));
        let rendered = |outcome: RunOutcome| {
            render_outcome(&block(outcome), &theme, 80, false)
                .into_iter()
                .map(|line| strip_terminal_sequences(&line))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let plain = rendered(with_warnings);
        assert_eq!(plain, rendered(completed), "{plain:?}");
        assert_eq!(plain, "✓ completed · 1m23s · 216 tok/s", "{plain:?}");
        assert!(!plain.to_ascii_lowercase().contains("warning"), "{plain:?}");
    }

    #[test]
    fn running_subagents_detail_appears_only_under_a_completed_turn() {
        let theme = crate::tui::theme::test_theme();
        let summary = RunSummary {
            files_changed: 0,
            tool_calls: 0,
            warnings: 0,
        };
        let completed = OutcomeBlock::new(
            RunOutcome::Completed {
                elapsed: Duration::from_secs(1),
                summary: summary.clone(),
            },
            None,
        );
        let with_warnings = OutcomeBlock::new(
            RunOutcome::CompletedWithWarnings {
                elapsed: Duration::from_secs(1),
                warnings: 3,
                summary: RunSummary {
                    warnings: 3,
                    ..summary.clone()
                },
            },
            None,
        );
        let lines = |block: &OutcomeBlock, running: bool| {
            render_outcome(block, &theme, 80, running)
                .into_iter()
                .map(|line| strip_terminal_sequences(&line))
                .collect::<Vec<_>>()
        };

        for block in [&completed, &with_warnings] {
            let idle = lines(block, false);
            assert_eq!(idle, vec!["✓ completed · 1.0s"], "{idle:?}");
            let live = lines(block, true);
            assert_eq!(
                live,
                vec![
                    "✓ completed · 1.0s",
                    "  subagents are running; inspect them in the /subagents menu.",
                ],
                "{live:?}"
            );
        }

        // The line wraps with the same two-space hanging indent in a narrow
        // terminal, and never appears under a failed or interrupted turn.
        let wrapped = render_outcome(&completed, &theme, 24, true)
            .into_iter()
            .map(|line| strip_terminal_sequences(&line))
            .collect::<Vec<_>>();
        assert!(wrapped.len() > 2, "{wrapped:?}");
        assert!(
            wrapped[1..].iter().all(|line| line.starts_with("  ")),
            "{wrapped:?}"
        );
        for outcome in [
            RunOutcome::Failed {
                elapsed: Duration::from_secs(1),
                reason: "command exited 1".into(),
            },
            RunOutcome::Interrupted {
                elapsed: Duration::from_secs(1),
            },
            RunOutcome::NeedsInput {
                prompt: "choose".into(),
            },
        ] {
            let rendered = render_outcome(&OutcomeBlock::new(outcome, None), &theme, 80, true)
                .into_iter()
                .map(|line| strip_terminal_sequences(&line))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!rendered.contains("subagents are running"), "{rendered:?}");
        }
    }

    #[test]
    fn failed_outcome_keeps_the_headline_and_shows_a_bounded_safe_reason() {
        let theme = crate::tui::theme::test_theme();
        let reason = format!(
            "\x1b[31mProvider unavailable\x1b[0m\x07\n{}",
            "é".repeat(MAX_OUTCOME_DETAIL_BYTES)
        );
        let outcome = RunOutcome::Failed {
            elapsed: Duration::from_millis(9400),
            reason,
        };

        assert_eq!(
            strip_terminal_sequences(&outcome_line(&outcome, None, &theme)),
            "× failed · 9.4s"
        );
        let RunOutcome::Failed { reason, .. } = &outcome else {
            unreachable!()
        };
        let detail = bounded_outcome_detail(reason);
        assert!(detail.starts_with("Provider unavailable␇\n"), "{detail:?}");
        assert!(detail.ends_with('…'));
        assert!(detail.len() <= MAX_OUTCOME_DETAIL_BYTES);
        assert!(detail.is_char_boundary(detail.len()));
        assert!(!detail.contains("\x1b[31m"));
        assert!(detail
            .chars()
            .all(|character| !character.is_control() || character == '\n'));

        let rendered = render_outcome(&OutcomeBlock::new(outcome.clone(), None), &theme, 48, false)
            .into_iter()
            .map(|line| strip_terminal_sequences(&line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.starts_with("× failed · 9.4s\n"), "{rendered:?}");
        assert!(rendered.contains("Provider unavailable␇"), "{rendered:?}");

        let copied = block_copy_text(&TranscriptBlock::Outcome(OutcomeBlock::new(outcome, None)));
        assert!(copied.starts_with("failed · 9.4s\nProvider unavailable␇\n"));
        assert!(copied.ends_with('…'));
    }
}

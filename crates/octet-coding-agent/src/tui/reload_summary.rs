//! One line for the outcome of a reload.
//!
//! A reload used to print one transcript notice per diagnostic. The user wants a
//! single note: a green dot for "reload succeeded", or a red dot for "reload
//! failed: reason". This module only builds the text; the dot comes from the
//! existing success/error notice tones.

/// Longest reason shown inline; longer ones are cut with an ellipsis.
const MAX_REASON_CHARS: usize = 160;
/// Reasons spelled out before the rest are counted.
const MAX_REASONS_SHOWN: usize = 2;

/// Text for a failed reload, or `None` when there were no problems.
///
/// Reasons are trimmed, flattened to one line, stripped of a leading
/// `warning:`/`error:` label (the dot already says it), de-duplicated in order,
/// and capped; anything beyond the cap is summarised as `(+N more)`.
pub(crate) fn reload_failure_text(problems: &[String]) -> Option<String> {
    let mut reasons: Vec<String> = Vec::new();
    for problem in problems {
        let reason = tidy(problem);
        if !reason.is_empty() && !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    if reasons.is_empty() {
        return None;
    }
    let extra = reasons.len().saturating_sub(MAX_REASONS_SHOWN);
    let shown = reasons
        .iter()
        .take(MAX_REASONS_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
    Some(if extra > 0 {
        format!("reload failed: {shown} (+{extra} more)")
    } else {
        format!("reload failed: {shown}")
    })
}

fn tidy(problem: &str) -> String {
    let flat = problem.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = flat.to_ascii_lowercase();
    let flat = ["warning:", "error:"]
        .iter()
        .find(|label| lowered.starts_with(**label))
        .map_or(flat.as_str(), |label| flat[label.len()..].trim_start())
        .to_owned();
    if flat.chars().count() > MAX_REASON_CHARS {
        let cut: String = flat.chars().take(MAX_REASON_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(problems: &[&str]) -> Option<String> {
        reload_failure_text(&problems.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn no_problems_is_a_success() {
        assert_eq!(text(&[]), None);
        assert_eq!(text(&["", "   \n "]), None);
    }

    #[test]
    fn one_problem_reads_as_one_short_reason_without_its_label() {
        assert_eq!(
            text(&["warning: skill `x` has no description"]).as_deref(),
            Some("reload failed: skill `x` has no description")
        );
        assert_eq!(
            text(&["Error:   theme\n  file unreadable"]).as_deref(),
            Some("reload failed: theme file unreadable")
        );
    }

    #[test]
    fn repeats_collapse_and_extras_are_counted() {
        assert_eq!(
            text(&["a", "b", "a", "c", "d", "b"]).as_deref(),
            Some("reload failed: a · b (+2 more)")
        );
    }

    #[test]
    fn a_long_reason_is_cut_to_one_line() {
        let long = "x".repeat(500);
        let summary = text(&[&long]).unwrap();
        assert!(summary.chars().count() < 200, "{summary}");
        assert!(summary.ends_with('…'));
        assert!(!summary.contains('\n'));
    }
}

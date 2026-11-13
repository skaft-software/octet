//! Bounded visual-row windows for compact command output.

/// Select a tail of already-wrapped physical rows. When either retained rows
/// or upstream capture loss must be disclosed, one physical row is reserved
/// for caller-rendered metadata. Short output stays short; `budget` is a hard
/// maximum, not a minimum height.
pub(super) fn bounded_tail_rows<F>(
    content: Vec<String>,
    budget: usize,
    force_metadata: bool,
    metadata: F,
) -> Vec<String>
where
    F: FnOnce(usize) -> String,
{
    if budget == 0 {
        return Vec::new();
    }

    let needs_metadata = force_metadata || content.len() > budget;
    let content_budget = budget.saturating_sub(usize::from(needs_metadata));
    let hidden_rows = content.len().saturating_sub(content_budget);
    let start = content.len().saturating_sub(content_budget);

    let mut rows = Vec::with_capacity(budget.min(content.len() + usize::from(needs_metadata)));
    if needs_metadata {
        rows.push(metadata(hidden_rows));
    }
    rows.extend(content.into_iter().skip(start));
    rows
}

/// Source-row boundaries shared by painting and semantic copy.
pub(super) fn head_tail_window(rows: usize, budget: usize) -> (usize, usize) {
    let retained = rows.min(budget.saturating_sub(1));
    let head = retained.div_ceil(2);
    (head, rows - (retained - head))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_window_preserves_both_ends_and_reserves_one_metadata_row() {
        // Eight rows in a five-row budget: two head, four hidden, two tail.
        assert_eq!(head_tail_window(8, 5), (2, 6));
        for budget in 0..=8 {
            let (head, tail_start) = head_tail_window(8, budget);
            let retained = head + 8 - tail_start;
            assert!(retained <= budget.saturating_sub(1), "budget {budget}");
            assert!(head <= tail_start);
        }
        // Short output keeps every row.
        assert_eq!(head_tail_window(1, 5), (1, 1));
    }

    #[test]
    fn compact_tail_reserves_metadata_inside_the_budget() {
        let rows = (1..=8).map(|row| format!("row {row}")).collect();
        let rendered = bounded_tail_rows(rows, 5, false, |hidden| format!("{hidden} hidden"));
        assert_eq!(rendered, ["4 hidden", "row 5", "row 6", "row 7", "row 8"]);
    }

    #[test]
    fn compact_tail_does_not_pad_short_output() {
        let rendered = bounded_tail_rows(vec!["one".into()], 5, false, |_| unreachable!());
        assert_eq!(rendered, ["one"]);
    }

    #[test]
    fn upstream_loss_forces_one_metadata_row_without_padding() {
        let rendered = bounded_tail_rows(vec!["one".into(), "two".into()], 5, true, |hidden| {
            format!("loss; {hidden} hidden")
        });
        assert_eq!(rendered, ["loss; 0 hidden", "one", "two"]);
    }
}

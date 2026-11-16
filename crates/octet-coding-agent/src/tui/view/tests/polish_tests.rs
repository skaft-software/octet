//! Exact, headless transcript presentation contracts.

use super::support::*;
use super::*;

fn screen_dump(rows: &[String], width: u16) -> String {
    let dump = emulate_rows(rows, width).screen().contents();
    println!("VT100 {width} columns:\n{dump}");
    dump
}

fn scroll_hint(scroll: usize, new: usize) -> String {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(120, 14);
    for number in 0..80 {
        shell.notice(format!("history {number:02}"));
    }
    shell.scroll(-1);
    {
        let mut state = shell.state.borrow_mut();
        state.scroll_from_bottom.set(scroll);
        state.viewport_anchor.set(None);
        state.new_output_count = new;
    }
    let rows = viewport::render_shell_viewport_at(&shell.state.borrow(), 120, Instant::now());
    let dump = screen_dump(&rows, 120);
    dump.lines()
        .find(|row| row.contains("back"))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn polish_scroll_counts_use_singular_and_plural_rows() {
    for (count, unit) in [(1, "row"), (2, "rows")] {
        assert_eq!(
            scroll_hint(count, 1),
            format!("↑ {count} {unit} back · 1 new · PageDown for latest")
        );
    }
}

#[test]
fn polish_count_nouns_share_one_rule() {
    let theme = crate::tui::theme::test_theme();
    for (count, unit) in [(0, "rows"), (1, "row"), (2, "rows")] {
        assert_eq!(plural(count, "row", "rows"), unit);
    }
    for (hidden, unit) in [(1, "row"), (2, "rows")] {
        let rows = tool_render::tool_input_preview(
            vec!["command".into(); 12 + hidden],
            "command",
            "",
            &theme,
            100,
            false,
        );
        let dump = screen_dump(&rows, 100);
        assert_eq!(
            dump.lines().last().unwrap(),
            format!("… {hidden} command {unit} hidden · Ctrl+O")
        );
    }
}

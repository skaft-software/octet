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

#[test]
fn polish_working_row_uses_the_theme_separator() {
    let mut working = AssistantBlock::streaming_reasoning("")
        .with_activity_started_at(Some(Instant::now() + Duration::from_secs(3600)));
    working.reasoning_heading = Some("Working".into());
    working.show_reasoning_hint = false;
    let theme = crate::tui::theme::test_theme();
    let renderer = theme.rich_renderer();
    let rows = render_block(
        None,
        &TranscriptBlock::Reasoning(Box::new(working)),
        &theme,
        &renderer,
        &renderer,
        120,
        false,
    );
    assert_eq!(
        screen_dump(&rows, 120).trim(),
        "• Working (0s · Esc to interrupt)"
    );
}

fn tool_panel(name: &str, args: serde_json::Value, output: &str, finished: bool) -> ToolPanel {
    ToolPanel::new(
        ToolCallId("polish-tool".into()),
        name.into(),
        args.to_string(),
        summarize_tool(name, &args),
        output.into(),
        finished,
        false,
        None,
        None,
    )
}

#[test]
fn polish_tool_omissions_share_hidden_word_and_shortcut() {
    let theme = crate::tui::theme::test_theme();
    let output = (1..=8)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let panel = tool_panel(
        "bash",
        serde_json::json!({"command":"run tests"}),
        &output,
        true,
    );
    let rows = render_compact_bash_output(&panel, &theme, 100, false, "");
    let dump = screen_dump(&rows, 100);
    assert_eq!(
        dump.lines().nth(2).unwrap(),
        "… 4 output rows hidden · Ctrl+O"
    );
}

#[test]
fn polish_output_window_keeps_two_head_and_two_nonblank_tail_rows() {
    let theme = crate::tui::theme::test_theme();
    let output = "line 1\n\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n\nline 8\n\n";
    for name in ["bash", "exec", "powershell", "codemode"] {
        let panel = tool_panel(
            name,
            serde_json::json!({"command":"run tests", "code":"return 1"}),
            output,
            true,
        );
        let rows = if name == "codemode" {
            codemode_render::project_output(&panel, &theme, 100, false).lines
        } else {
            render_compact_bash_output(&panel, &theme, 100, false, "")
        };
        assert_eq!(
            screen_dump(&rows, 100),
            "line 1\nline 2\n… 4 output rows hidden · Ctrl+O\nline 7\nline 8",
            "{name}"
        );
    }
}

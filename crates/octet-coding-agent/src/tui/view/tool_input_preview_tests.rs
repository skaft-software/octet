//! Regression coverage for width-bounded shell and generic input disclosure.

use octet_ai::ToolCallId;
use sexy_tui_rs::{strip_terminal_sequences, visible_width};

use super::*;
use crate::presentation::summarize_tool;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crate::tui::theme::{test_theme, test_theme_from_source, test_theme_with};
use crate::tui::view::{
    transcript_render::render_block, transcript_selection::block_copy_text, TranscriptBlock,
};

fn block(name: &str, input: &str, finished: bool, failed: bool) -> TranscriptBlock {
    let args = serde_json::json!({
        "command": input,
        "token": "private-unselected-field",
        "nested": {"command": "private-nested-field"},
    });
    TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("input-preview".into()),
        name.into(),
        args.to_string(),
        summarize_tool(name, &args),
        String::new(),
        finished,
        failed,
        failed.then(|| "permission denied".into()),
        None,
    )))
}

fn render(block: &TranscriptBlock, theme: &OctetTheme, width: u16, expanded: bool) -> Vec<String> {
    let renderer = theme.rich_renderer();
    let rows = render_block(None, block, theme, &renderer, &renderer, width, expanded);
    assert!(rows
        .iter()
        .all(|row| visible_width(row) <= usize::from(width)));
    if theme.capabilities().color == ColorDepth::None {
        assert!(rows.iter().all(|row| !row.contains('\x1b')));
    }
    rows.iter()
        .map(|row| strip_terminal_sequences(row))
        .collect()
}

#[test]
fn tool_input_preview_long_single_line_is_not_cut_after_72_characters() {
    let input = format!("printf {} END_MARKER", "x".repeat(96));
    for name in ["bash", "powershell", "remote_shell"] {
        for (finished, failed) in [(false, false), (true, false), (true, true)] {
            let block = block(name, &input, finished, failed);
            let theme = test_theme();
            let collapsed = render(&block, &theme, 46, false);
            let expanded = render(&block, &theme, 46, true);
            assert_eq!(
                collapsed, expanded,
                "short wrapped input must stay complete"
            );
            let text = collapsed.join("\n");
            assert!(text.contains("END_MARKER"), "{name}: {text}");
            assert!(!text.contains("Ctrl+O"), "{text}");
            assert!(block_copy_text(&block).contains(&input));
            assert!(!text.contains("private-"));
        }
    }
}

#[test]
fn tool_input_preview_multiline_uses_six_rows_in_active_success_and_failure_views() {
    let six = "printf one\n\n    printf three\nprintf four\nprintf five\nprintf six";
    let eight = format!("{six}\nprintf seventh\nprintf eighth");
    for name in ["bash", "powershell", "remote_shell"] {
        for input in [six, &eight] {
            for (finished, failed) in [(false, false), (true, false), (true, true)] {
                let block = block(name, input, finished, failed);
                let theme = test_theme();
                let collapsed = render(&block, &theme, 120, false);
                let expanded = render(&block, &theme, 120, true);
                assert_eq!(&collapsed[..6], &expanded[..6], "{name}");
                assert!(collapsed[1].trim().is_empty() || collapsed[1].contains('│'));
                assert!(collapsed[2].contains("    printf three"));
                assert!(collapsed[5].contains("printf six"));
                if input == six {
                    assert_eq!(collapsed, expanded);
                    assert!(!collapsed.join("\n").contains("Ctrl+O"));
                } else {
                    let kind = if name == "remote_shell" {
                        "input"
                    } else {
                        "command"
                    };
                    assert!(collapsed[6].contains(&format!("2 {kind} rows hidden")));
                    assert!(collapsed[6].contains("Ctrl+O"));
                    assert!(!collapsed.join("\n").contains("seventh"));
                    assert!(expanded[6].contains("printf seventh"));
                    assert!(expanded[7].contains("printf eighth"));
                }
                if failed {
                    assert!(collapsed.join("\n").contains("permission denied"));
                }
                assert!(block_copy_text(&block).contains(input));
                assert!(!block_copy_text(&block).contains("private-"));
                assert!(!block_copy_text(&block).contains("Ctrl+O"));
                assert_eq!(collapsed, render(&block, &theme, 120, false));
            }
        }
    }
}

#[test]
fn tool_input_preview_counts_wrapped_unicode_rows_and_reflows_without_splitting_graphemes() {
    let input = "界e\u{301}👩‍💻".repeat(200);
    for color in [
        ColorDepth::None,
        ColorDepth::Ansi16,
        ColorDepth::Ansi256,
        ColorDepth::TrueColor,
    ] {
        let theme = test_theme_with(TerminalCapabilities::test(true, true, color));
        for name in ["bash", "powershell", "remote_shell"] {
            for width in [42, 80, 120] {
                let block = block(name, &input, true, false);
                let collapsed = render(&block, &theme, width, false);
                let expanded = render(&block, &theme, width, true);
                let full_rows = expanded.iter().filter(|row| row.contains('界')).count();
                assert!(full_rows > 6, "fixture must exceed six visible rows");
                assert_eq!(collapsed[..6], expanded[..6]);
                assert!(collapsed[6].contains("Ctrl+O"));
                let hidden = full_rows - 6;
                assert!(
                    collapsed[6].contains(&hidden.to_string()),
                    "{:?}",
                    collapsed[6]
                );
                for row in &expanded {
                    let first = row.trim_start_matches([' ', '│']);
                    assert!(!first.starts_with(['\u{301}', '\u{200d}']));
                    assert!(!row.ends_with('\u{200d}'));
                }
                assert!(block_copy_text(&block).contains(&input));
            }
        }
    }
}

#[test]
fn tool_input_preview_quiet_themes_do_not_clip_inputs_back_to_one_line() {
    let theme = test_theme_from_source("[tokens]\nquiet_tool_summaries = true\n");
    let input = "printf first\nprintf second\nprintf third\nprintf fourth\nprintf fifth\nprintf sixth\nprintf seventh";
    for name in ["bash", "powershell", "remote_shell"] {
        let block = block(name, input, false, false);
        let collapsed = render(&block, &theme, 120, false);
        assert!(collapsed[5].contains("printf sixth"));
        assert!(collapsed[6].contains("1 "));
        assert!(collapsed[6].contains("Ctrl+O"));
        assert!(!collapsed.join("\n").contains("printf seventh"));
        assert!(render(&block, &theme, 120, true)
            .join("\n")
            .contains("printf seventh"));
    }
}

#[test]
fn tool_input_preview_sanitizes_selected_input_without_exposing_raw_arguments() {
    let input = "printf first\n\n\tprintf third\nprintf fourth\nprintf fifth\nprintf sixth\nprintf '\x1b]52;c;literal-osc-payload\x07\x1b[2J'";
    for unicode in [false, true] {
        for name in ["bash", "powershell", "remote_shell"] {
            let theme =
                test_theme_with(TerminalCapabilities::test(true, unicode, ColorDepth::None));
            let block = block(name, input, true, false);
            for expanded in [false, true] {
                let text = render(&block, &theme, 80, expanded).join("\n");
                assert!(!text.chars().any(|ch| ch.is_control() && ch != '\n'));
                assert!(!text.contains("private-"));
                assert!(!text.contains("\"command\":"));
            }
            let copied = block_copy_text(&block);
            assert!(!copied.contains("private-"));
            assert!(!copied.contains('\x1b'));
            assert!(copied.contains("printf sixth"));
        }
    }
}

#[test]
fn tool_input_preview_global_disclosure_reveals_and_recovers_live_and_completed_inputs() {
    use crate::tui::view::InteractiveShell;
    use octet_agent::{AgentEvent, ToolOutput};
    use std::time::Duration;

    let input = "one\ntwo\nthree\nfour\nfive\nsix\nLAST_COMMAND";
    for name in ["bash", "powershell", "remote_shell"] {
        let mut shell = InteractiveShell::test_shell();
        let id = ToolCallId("live-input".into());
        shell.on_agent_event(&AgentEvent::ToolStarted {
            id: id.clone(),
            name: name.into(),
            args: serde_json::json!({"command": input}),
        });
        let transcript = |shell: &InteractiveShell| {
            strip_terminal_sequences(&shell.state.borrow().rendered_transcript(100).join("\n"))
        };
        for finished in [false, true] {
            if finished {
                shell.on_agent_event(&AgentEvent::ToolFinished {
                    id: id.clone(),
                    result: Ok(ToolOutput::new("exit=0 duration=0.1s\n(no output)")),
                    duration: Duration::from_millis(100),
                });
            }
            let collapsed = transcript(&shell);
            assert!(collapsed.contains("Ctrl+O"));
            assert!(!collapsed.contains("LAST_COMMAND"));
            shell.toggle_disclosure();
            let expanded = transcript(&shell);
            assert!(expanded.contains("LAST_COMMAND"), "{name}: {expanded}");
            assert!(!expanded.contains("hidden"));
            shell.toggle_disclosure();
            assert_eq!(collapsed, transcript(&shell));
        }
    }
}

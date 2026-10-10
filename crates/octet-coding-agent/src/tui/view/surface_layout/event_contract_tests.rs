//! Regressions exercise emitted ANSI rows, not just surface-plan arithmetic.
use std::time::Duration;

use sexy_tui_rs::{strip_terminal_sequences, visible_width};

use super::super::{render_block_planned, AssistantBlock, NoticeTone, OutcomeBlock, ToolPanel};
use super::*;
use crate::presentation::{summarize_tool, RunOutcome, RunSummary};
use crate::tui::theme::{test_theme, test_theme_from_source};

fn themes() -> Vec<(String, OctetTheme)> {
    let mut themes = vec![
        ("default".into(), test_theme()),
        (
            "Cards".into(),
            test_theme_from_source(include_str!("../../../../../../examples/themes/Cards.toml")),
        ),
    ];
    // Synthetic recipes intentionally vary independently of the installed
    // palettes. Spacing has no theme key, including in content-width themes.
    for (index, chrome) in ["plain", "band", "rail", "card", "rule"].iter().enumerate() {
        for prompt_padding in [false, true] {
            let mut source = format!(
                "[layout]\nprompt_padding = {prompt_padding}\ndensity = \"{}\"\n",
                ["compact", "comfortable", "airy"][index % 3],
            );
            for kind in [
                "user",
                "reasoning",
                "tool",
                "outcome",
                "notice",
                "assistant",
            ] {
                source.push_str(&format!(
                    "[surfaces.{kind}]\nchrome = \"{chrome}\"\npadding = {}\nwidth = \"{}\"\n",
                    index % 2,
                    if index % 2 == 0 { "content" } else { "full" },
                ));
            }
            themes.push((
                format!("{chrome}/prompt_padding={prompt_padding}"),
                test_theme_from_source(&source),
            ));
        }
    }
    themes
}

fn prompt() -> TranscriptBlock {
    TranscriptBlock::User {
        text: "Ready for work".into(),
        model_lab: None,
        prompt_color: Some("#123456".into()),
        persisted: true,
    }
}

fn working() -> TranscriptBlock {
    let mut activity = AssistantBlock::streaming_reasoning("");
    activity.reasoning_heading = Some("Working".into());
    activity.show_reasoning_hint = false;
    TranscriptBlock::Reasoning(Box::new(activity))
}

fn thinking() -> TranscriptBlock {
    TranscriptBlock::Reasoning(Box::new(AssistantBlock::streaming_reasoning(
        "private detail",
    )))
}

fn tool() -> TranscriptBlock {
    let args = serde_json::json!({"command": "echo ready"});
    TranscriptBlock::Tool(Box::new(ToolPanel::new(
        octet_ai::ToolCallId("spacing-regression".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        "ready".into(),
        true,
        false,
        None,
        None,
    )))
}

fn completion() -> TranscriptBlock {
    TranscriptBlock::Outcome(OutcomeBlock::new(
        RunOutcome::Completed {
            elapsed: Duration::from_secs(83),
            summary: RunSummary {
                files_changed: 0,
                tool_calls: 1,
                warnings: 0,
            },
        },
        None,
    ))
}

fn byte_rows(blocks: &[&TranscriptBlock], theme: &OctetTheme, width: u16) -> Vec<String> {
    let renderer = theme.rich_renderer();
    let reasoning = theme.reasoning_renderer();
    let mut bytes = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let rendered = render_block_planned(
            index.checked_sub(1).map(|previous| blocks[previous]),
            block,
            theme,
            &renderer,
            &reasoning,
            width,
            false,
            1,
            7,
        );
        for row in rendered.lines {
            assert!(
                visible_width(&row) <= usize::from(width),
                "{width}: {row:?}"
            );
            bytes.extend_from_slice(row.as_bytes());
            bytes.extend_from_slice(b"\r\n");
        }
    }
    String::from_utf8(bytes)
        .expect("renderer emits UTF-8")
        .strip_suffix("\r\n")
        .unwrap_or("")
        .split("\r\n")
        .map(strip_terminal_sequences)
        .collect()
}

fn blank_runs(rows: &[String], needle: &str) -> (usize, usize) {
    let index = rows
        .iter()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle:?}: {rows:?}"));
    let before = rows[..index]
        .iter()
        .rev()
        .take_while(|row| row.trim().is_empty())
        .count();
    let after = rows[index + 1..]
        .iter()
        .take_while(|row| row.trim().is_empty())
        .count();
    (before, after)
}

#[test]
fn event_blanks_are_theme_independent_and_working_promotion_does_not_bounce() {
    let prompt = prompt();
    let working = working();
    let thinking = thinking();
    let tool = tool();
    let completed = completion();
    let notice = TranscriptBlock::Notice("Notice marker".into());
    let status = TranscriptBlock::NoticeStatus {
        text: "Status marker".into(),
        tone: NoticeTone::Success,
        reserved_rows: 0,
    };
    let prose =
        TranscriptBlock::Assistant(Box::new(AssistantBlock::finalized("Answer marker".into())));
    for (name, theme) in themes() {
        for width in [32, 80, 120] {
            let initial = byte_rows(&[&prompt, &working], &theme, width);
            assert_eq!(
                blank_runs(&initial, "Working"),
                (1, 1),
                "{name}/{width}: {initial:?}"
            );
            let promoted = byte_rows(&[&prompt, &thinking], &theme, width);
            assert_eq!(
                blank_runs(&promoted, "Thinking").0,
                1,
                "{name}/{width}: {promoted:?}"
            );
            assert_eq!(
                initial.len(),
                promoted.len(),
                "{name}/{width}: composer promotion bounce"
            );

            let handoff = byte_rows(&[&prompt, &thinking, &working], &theme, width);
            assert_eq!(
                blank_runs(&handoff, "Working"),
                (1, 1),
                "{name}/{width}: {handoff:?}"
            );
            let tools_live = byte_rows(&[&prompt, &tool, &working], &theme, width);
            assert_eq!(
                blank_runs(&tools_live, "Working"),
                (1, 1),
                "{name}/{width}: {tools_live:?}"
            );
            let settled = byte_rows(
                &[&prompt, &tool, &completed, &notice, &status, &prose],
                &theme,
                width,
            );
            for needle in ["completed", "Notice marker", "Status marker"] {
                assert_eq!(
                    blank_runs(&settled, needle),
                    (1, 1),
                    "{name}/{width}/{needle}: {settled:?}"
                );
            }
            assert!(
                settled.iter().any(|row| row.contains("1m23s")),
                "{name}/{width}: {settled:?}"
            );
        }
    }
}

#[test]
fn filled_neighbor_cushions_collapse_with_host_event_spacing_in_both_directions() {
    let theme = test_theme_from_source(
        "[layout]\nprompt_padding = true\ndensity = \"airy\"\n\
         [surfaces.user]\nchrome = \"band\"\npadding = 1\n\
         [surfaces.tool]\nchrome = \"band\"\npadding = 1\n\
         [surfaces.notice]\nchrome = \"band\"\npadding = 1\n",
    );
    let prompt = prompt();
    let working = working();
    let completed = completion();
    let notice = TranscriptBlock::Notice("Notice marker".into());
    let tool = tool();
    for width in [32, 80, 120] {
        let rows = byte_rows(&[&prompt, &working], &theme, width);
        assert_eq!(blank_runs(&rows, "Working"), (1, 1), "{rows:?}");
        let rows = byte_rows(&[&tool, &completed, &notice, &prompt], &theme, width);
        for needle in ["completed", "Notice marker"] {
            assert_eq!(blank_runs(&rows, needle), (1, 1), "{rows:?}");
        }
    }
}

#[test]
fn content_width_uses_usable_streaming_width_and_real_final_markdown_and_completion() {
    for chrome in ["plain", "band", "rail", "card"] {
        let theme = test_theme_from_source(&format!(
            "[surfaces.assistant]\nchrome = \"{chrome}\"\npadding = 1\nwidth = \"content\"\n\
             [surfaces.outcome]\nchrome = \"{chrome}\"\npadding = 1\nwidth = \"content\"\n",
        ));
        let completed = completion();
        for width in [32, 80, 120] {
            let mut streaming = TranscriptBlock::Assistant(Box::new(AssistantBlock::streaming("")));
            let initial = compile_surface_plan(None, &streaming, &theme, width);
            assert!(
                initial.geometry.content_width > 12,
                "{chrome}/{width}: {initial:?}"
            );
            let TranscriptBlock::Assistant(assistant) = &mut streaming else {
                unreachable!()
            };
            assistant.append(
                "Visible prose keeps the preferred usable reading width and the final marker.",
            );
            let streamed = compile_surface_plan(None, &streaming, &theme, width);
            assert_eq!(
                initial.geometry.content_width,
                streamed.geometry.content_width
            );
            let rows = byte_rows(&[&streaming], &theme, width);
            assert!(rows.iter().any(|row| row.contains("marker.")), "{rows:?}");

            let numbered = TranscriptBlock::Assistant(Box::new(AssistantBlock::finalized(
                "1. First numbered item stays visible.\n2. Second numbered item has a final marker.\n3. Third item.".into(),
            )));
            let plan = compile_surface_plan(None, &numbered, &theme, width);
            assert!(
                plan.geometry.content_width > 12,
                "{chrome}/{width}: {plan:?}"
            );
            let rows = byte_rows(&[&numbered, &completed], &theme, width);
            for marker in [
                "1.",
                "2.",
                "3.",
                "visible.",
                "marker.",
                "completed",
                "1m23s",
            ] {
                assert!(
                    rows.iter().any(|row| row.contains(marker)),
                    "{chrome}/{width}/{marker}: {rows:?}"
                );
            }

            // Content width remains meaningful: finite short replies/cards do
            // not become terminal-wide as a workaround for streaming width.
            let short = TranscriptBlock::Assistant(Box::new(AssistantBlock::finalized(
                "Short reply".into(),
            )));
            let short_plan = compile_surface_plan(None, &short, &theme, width);
            assert!(
                short_plan.frame_width < initial.frame_width,
                "{chrome}/{width}: {short_plan:?}"
            );
        }
    }
}

#[test]
fn content_width_incremental_rows_match_full_rows_and_committed_semantics() {
    use super::super::transcript_render::render_assistant_update_planned;

    let theme = test_theme_from_source(
        "[surfaces.assistant]\nchrome = \"card\"\npadding = 1\nwidth = \"content\"\n",
    );
    let renderer = theme.rich_renderer();
    let reasoning = theme.reasoning_renderer();
    for width in [32, 80, 120] {
        let mut block = TranscriptBlock::Assistant(Box::new(AssistantBlock::streaming(
            "A stable paragraph with visible words.\n\nMutable tail",
        )));
        let first = render_block_planned(
            None, &block, &theme, &renderer, &reasoning, width, false, 0, 0,
        );
        let TranscriptBlock::Assistant(assistant) = &mut block else {
            unreachable!()
        };
        assistant.append(" grows without losing the ending marker.");
        let update = render_assistant_update_planned(
            None, &block, &theme, &renderer, &reasoning, width, false,
        )
        .expect("committed paragraph supplies a stable prefix");
        let mut incremental = first.lines[..update.stable_rows].to_vec();
        incremental.extend(update.replacement);
        let full = render_block_planned(
            None, &block, &theme, &renderer, &reasoning, width, false, 0, 0,
        );
        assert_eq!(
            incremental, full.lines,
            "{width}: retained/committed row mismatch"
        );
        let copy = block_copy_text(&block);
        let TranscriptBlock::Assistant(assistant) = &mut block else {
            unreachable!()
        };
        assistant.finish();
        let final_rows = byte_rows(&[&block], &theme, width);
        assert!(
            final_rows.iter().any(|row| row.contains("marker.")),
            "{final_rows:?}"
        );
        assert_eq!(block_copy_text(&block), copy);
    }
}

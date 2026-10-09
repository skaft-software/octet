//! Rich markdown and code surfaces, colour modes, and bash card windowing. Separate because
//! they assert what the rich pipeline and the compact bash renderer do to the bytes they emit.

use super::support::*;

use super::*;

#[test]
fn wrapped_tool_summaries_keep_their_action_indent() {
    let theme = crate::tui::theme::test_theme();
    let args = serde_json::json!({
        "path": "crates/octet-coding-agent/src/tui/a-very-long-file-name-that-must-wrap-without-losing-the-tool-label.rs"
    });
    let panel = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("id".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        String::new(),
        false,
        false,
        None,
        None,
    )));
    let renderer = theme.rich_renderer();
    let lines = render_block(None, &panel, &theme, &renderer, &renderer, 80, false)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();

    assert!(lines.len() > 1, "the long summary should wrap: {lines:?}");
    assert!(lines[0].starts_with("• Read"), "{lines:?}");
    let value_byte = lines[0]
        .find("crates/octet-coding-agent")
        .expect("tool summary value on first row");
    let value_column = visible_width(&lines[0][..value_byte]);
    let continuation_indent = " ".repeat(value_column);
    assert!(
        lines[1..]
            .iter()
            .filter(|line| !line.is_empty())
            .all(|line| line.starts_with(&continuation_indent)),
        "continuations must hang under the summary column: {lines:?}"
    );
    assert!(lines.last().is_some_and(|line| !line.is_empty()));
}

#[test]
fn default_rich_markdown_is_copy_safe_on_an_unknown_background() {
    let theme = crate::tui::theme::test_theme();
    let source = concat!(
        "Use `Session` without a painted chip.\n\n",
        "```text\nclone/read-only projection\nterminal rows\n```"
    );
    let rendered =
        AssistantBlock::finalized(source.into()).render(&theme.rich_renderer(), &theme, 160);
    let joined = rendered.join("\n");
    assert!(!joined.contains("\x1b[48;"), "{joined:?}");
    assert!(!joined.contains("```"), "{joined}");
    let copied = strip_terminal_sequences(&joined);
    assert!(copied.contains("clone/read-only projection"));
    assert!(copied.contains("terminal rows"));
    assert!(
        !copied.chars().any(|ch| "┌┐└┘╭╮╰╯│─".contains(ch)),
        "{copied:?}"
    );
}

#[test]
fn compiled_default_code_surfaces_adapt_and_cover_language_padding() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    for (background, sequence) in [
        (TerminalBackground::Dark, "\x1b[48;2;32;38;48m"),
        (TerminalBackground::Light, "\x1b[48;2;241;245;244m"),
    ] {
        let theme = crate::tui::theme::test_theme_for(background, capabilities);
        let rendered = AssistantBlock::finalized("```rust\nlet answer = 42;\n```".into()).render(
            &theme.rich_renderer(),
            &theme,
            80,
        );
        assert!(rendered.len() >= 2, "{background:?}: {rendered:?}");
        assert!(
            rendered.iter().all(|line| line.contains(sequence)),
            "{background:?}: {rendered:?}"
        );
        let widths = rendered
            .iter()
            .map(|line| visible_width(line))
            .collect::<Vec<_>>();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "{widths:?}"
        );
        let copied = strip_terminal_sequences(&rendered.join("\n"));
        assert!(!copied.chars().any(|ch| "┌┐└┘╭╮╰╯│─".contains(ch)));
    }
}

#[test]
fn streamed_markdown_settles_into_rich_structure() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "## Session recovery\n\n**Changes**\n- preserves ".into(),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "valid records\n- removes invalid bytes".into(),
        },
    );
    // Finalization performs the authoritative CommonMark parse. The live
    // suffix remains deliberately literal until its boundary is proven.
    shell.state.borrow_mut().close_streaming_blocks();
    {
        let state = shell.state.borrow();
        let TranscriptBlock::Assistant(assistant) = &state.transcript[0] else {
            panic!("first block must be assistant Markdown");
        };
        assert!(assistant.markdown.is_finished());
        assert_eq!(
            assistant.markdown.committed(),
            &sexy_tui_rs::parse_markdown(&assistant.text)
        );
    }
    let rendered = render_shell(&shell.state.borrow(), 60).join("\n");
    for raw in ["##", "**", "- preserves"] {
        assert!(!rendered.contains(raw), "raw marker leaked: {rendered}");
    }
    assert!(rendered.contains("Session recovery"));
    assert!(rendered.contains('—'));
}

#[test]
fn colour_modes_confirmation_survives_model_resize_and_repeated_frames() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
        ColorDepth::None,
    ] {
        for unicode in [false, true] {
            for background in [TerminalBackground::Dark, TerminalBackground::Light] {
                let theme = crate::tui::theme::test_theme_for(
                    background,
                    TerminalCapabilities::test(true, unicode, depth),
                );
                let mut shell = InteractiveShell::test_shell_with_theme(theme);
                shell.set_size(80, 24);
                shell.open_panel(Panel::SelectList {
                    surface: OrdinarySurfaceMetadata::new("Approve tool effect?"),
                    items: vec!["Deny".into(), "Approve".into()],
                    descriptions: vec![Some("writes src/lib.rs".into()); 2],
                    selected: 0,
                    filter: String::new(),
                    action: PanelAction::Confirmation,
                });
                shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
                shell.panel_input(&panel_key(crossterm::event::KeyCode::Char('x')));
                for (provider, model) in [("anthropic", "claude-sonnet-4"), ("openai", "gpt-5.6")] {
                    shell.set_identity(provider, model, "off");
                    for width in [40, 80, 160, 40] {
                        shell.set_size(width, 24);
                        let frame = render_panel(&shell.state.borrow(), width);
                        assert_eq!(frame, render_panel(&shell.state.borrow(), width));
                        let text = strip_terminal_sequences(&frame.join("\n"));
                        assert!(text.contains("Deny") && text.contains("Approve"));
                        assert_eq!(text.matches("writes src/lib.rs").count(), 1);
                        assert!(!text.contains("Filter"));
                        assert!(panel_state(&shell).2.is_empty());
                        if !unicode {
                            assert!(text.is_ascii(), "{text:?}");
                        }
                        if depth == ColorDepth::None {
                            assert!(!frame.join("").contains('\x1b'));
                        }
                    }
                }
                shell.set_size(80, 5);
                assert!(shell
                    .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
                    .is_none());
                assert!(shell.has_panel());
                shell.set_size(80, 6);
                let (result, action) = shell
                    .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
                    .expect("resized selected action is visible");
                assert_eq!(result, PanelResult::Confirm(1));
                assert!(matches!(action, PanelAction::Confirmation));
                shell.close_panel();
                shell
                    .state
                    .borrow_mut()
                    .push_block(TranscriptBlock::Outcome(OutcomeBlock::new(
                        RunOutcome::Completed {
                            elapsed: Duration::from_secs(1),
                            summary: crate::presentation::RunSummary {
                                files_changed: 1,
                                tool_calls: 1,
                                warnings: 0,
                            },
                        },
                        None,
                    )));
                shell
                    .state
                    .borrow_mut()
                    .push_block(TranscriptBlock::Outcome(OutcomeBlock::new(
                        RunOutcome::Failed {
                            elapsed: Duration::from_secs(2),
                            reason: "permission denied".into(),
                        },
                        None,
                    )));
                shell.set_size(80, 24);
                let frame = render_shell(&shell.state.borrow(), 80);
                assert_eq!(frame, render_shell(&shell.state.borrow(), 80));
                let text = strip_terminal_sequences(&frame.join("\n"));
                for label in ["completed", "failed", "permission denied"] {
                    assert!(text.contains(label), "{text:?}");
                }
                if !unicode {
                    assert!(text.replace(CURSOR_MARKER, "").is_ascii(), "{text:?}");
                }
            }
        }
    }
}

#[test]
fn full_tui_colour_modes_preserve_readable_content_and_supported_encoding() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    for depth in [
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
        ColorDepth::None,
    ] {
        for background in [TerminalBackground::Dark, TerminalBackground::Light] {
            for width in [40, 80, 160] {
                let capabilities = TerminalCapabilities::test(true, true, depth);
                let theme = crate::tui::theme::test_theme_for(background, capabilities);
                let mut shell = InteractiveShell::test_shell_with_theme(theme);
                shell.set_size(width, 24);
                shell.set_identity("anthropic", "claude-sonnet-4", "high");
                let mut samples = render_shell(&shell.state.borrow(), width);
                shell.state.borrow_mut().push_block(TranscriptBlock::Assistant(Box::new(
                    AssistantBlock::finalized("# Palette check\n\n**Strong** and `inline`.\n\n- list item\n\n```rust\nlet answer = 42;\n```\n\n| State | Value |\n| --- | --- |\n| ready | 42 |".into()),
                )));
                samples.extend(render_shell(&shell.state.borrow(), width));
                open_select_panel(&mut shell, &["Ready", "Approval required", "Failed"]);
                samples.extend(render_shell(&shell.state.borrow(), width));
                shell.close_panel();
                {
                    let state = shell.state.borrow();
                    let theme = &state.theme;
                    samples.extend(AssistantBlock::finalized(
                        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new".into(),
                    ).render(&theme.rich_renderer(), theme, width));
                }
                let joined = samples.join("\n");
                let visible = strip_terminal_sequences(&joined);
                assert!(visible.contains("-old") && visible.contains("+new"));
                assert!(visible.contains("Palette check"));
                assert!(
                    visible.contains("Palette check") && visible.contains("answer"),
                    "{depth:?}/{width}: {visible}"
                );
                assert!(
                    visible.contains("Approval required"),
                    "{depth:?}/{width}: {visible}"
                );
                if depth != ColorDepth::TrueColor {
                    assert!(!joined.contains("38;2;") && !joined.contains("48;2;"));
                }
                if matches!(depth, ColorDepth::None | ColorDepth::Ansi16) {
                    assert!(!joined.contains("38;5;") && !joined.contains("48;5;"));
                }
                if depth == ColorDepth::None {
                    assert!(!joined.contains("\x1b["), "no-colour SGR: {joined:?}");
                }
                if depth == ColorDepth::Ansi256 {
                    assert!(joined.contains("38;5;") && joined.contains("48;5;"));
                    for escape in joined.split("\x1b[").skip(1) {
                        let Some((sgr, _)) = escape.split_once('m') else {
                            continue;
                        };
                        let codes = sgr.split(';').collect::<Vec<_>>();
                        for triple in codes.windows(3) {
                            if matches!(triple[0], "38" | "48") && triple[1] == "5" {
                                assert!(
                                    triple[2].parse::<u8>().unwrap() >= 16,
                                    "theme-owned slot: {sgr}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn compiled_default_composer_keeps_the_terminal_background_unfilled() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    for background in [
        TerminalBackground::Dark,
        TerminalBackground::Light,
        TerminalBackground::Unknown,
    ] {
        let theme = crate::tui::theme::test_theme_for(background, capabilities);
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.set_identity("anthropic", "claude-sonnet-4", "high");
        let rendered = crate::tui::composer_surface::render_composer_surface(
            &shell.state.borrow(),
            120,
            Instant::now(),
        )
        .join("\n");
        assert!(rendered.contains("38;2;"), "{background:?}: {rendered:?}");
        assert!(
            !rendered.contains("\x1b[48;2;"),
            "{background:?}: {rendered:?}"
        );
    }
}

#[test]
fn compiled_default_composer_border_is_static_during_work() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    let theme = crate::tui::theme::test_theme_for(TerminalBackground::Dark, capabilities);
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    let now = Instant::now();
    let idle_before =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 80, now);
    let idle_after = crate::tui::composer_surface::render_composer_surface(
        &shell.state.borrow(),
        80,
        now + Duration::from_secs(5),
    );
    assert_eq!(idle_before[0], idle_after[0]);

    let run_id = shell.begin_run("anthropic");
    let accent = {
        let state = shell.state.borrow();
        state
            .theme
            .model_rgb(Some(ModelLab::Anthropic))
            .expect("Anthropic model accent")
    };
    let active_before =
        crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), 80, now);
    let active_after = crate::tui::composer_surface::render_composer_surface(
        &shell.state.borrow(),
        80,
        now + Duration::from_secs(5),
    );
    assert_eq!(active_before[0], active_after[0]);
    assert_eq!(idle_before[0], active_before[0]);
    assert!(active_before[0].contains(&format!("38;2;{};{};{}", accent.0, accent.1, accent.2)));
    assert!(active_before[..3]
        .iter()
        .chain(&active_after[..3])
        .all(|line| !line.contains("\x1b[48;2;")));

    shell.interrupt_run(run_id);
    let rest = crate::tui::composer_surface::render_composer_surface(
        &shell.state.borrow(),
        80,
        now + Duration::from_secs(10),
    );
    assert_eq!(idle_before[0], rest[0]);
}

#[test]
fn scripted_agent_events_map_to_distinct_transcript_and_tool_state() {
    use octet_agent::{EntryId, FinishReason, ToolOutput};
    use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol};

    let mut shell = InteractiveShell::test_shell();
    let id = ToolCallId("call-1".into());
    let events = vec![
        AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "considering".into(),
        },
        AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "answer".into(),
        },
        AgentEvent::ToolStarted {
            id: id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path": "src/lib.rs"}),
        },
        AgentEvent::ToolProgress {
            id: id.clone(),
            progress: ToolProgress::Status("reading".into()),
        },
        AgentEvent::ToolFinished {
            id: id.clone(),
            result: Ok(ToolOutput::new("contents")),
            duration: Duration::from_millis(10),
        },
        AgentEvent::TurnFinished {
            turn_cost: None,
            message: AssistantMessage {
                content: vec![AssistantPart::Text("answer".into())],
                model: ModelId("m".into()),
                protocol: Protocol::OpenAiChat,
            },
            stop_reason: octet_ai::StopReason::EndTurn,
            turn_usage: Usage {
                input_tokens: 12,
                output_tokens: 3,
                total_tokens: 15,
                ..Usage::default()
            },
            usage: Usage {
                input_tokens: 12,
                output_tokens: 3,
                total_tokens: 15,
                ..Usage::default()
            },
            session_cost_microdollars: Some(4200),
            run_cost_microdollars: 4200,
        },
        AgentEvent::RunFinished {
            head: EntryId("003".into()),
            reason: FinishReason::Completed,
        },
    ];
    for event in &events {
        shell.on_agent_event(event);
    }
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("considering"));
    assert!(snapshot.contains("answer"));
    assert!(snapshot.contains("read"));
    assert_eq!(shell.debug_tool_output(&id).as_deref(), Some("contents"));
}

#[test]
fn active_bash_renders_command_and_latest_output_tail() {
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    let id = ToolCallId("live-bash".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "bash".into(),
            args: serde_json::json!({"command": "long-running-check"}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolProgress {
            id: id.clone(),
            progress: ToolProgress::Output {
                stream: octet_agent::OutputStream::Stdout,
                bytes: bytes::Bytes::from_static(b"private live output"),
            },
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolProgress {
            id,
            progress: ToolProgress::Status("private status detail".into()),
        },
    );
    let rendered = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 100).join("\n"));
    assert!(rendered.contains("Bash  long-running-check"), "{rendered}");
    assert!(rendered.contains("private live output"), "{rendered}");
    assert!(rendered.contains("private status detail"), "{rendered}");
}

#[test]
fn bash_wraps_command_and_nests_output_under_one_elbow() {
    let theme = crate::tui::theme::test_theme();
    let command = "node --input-type=module --check < octet/demo.js && git diff --check";
    let args = serde_json::json!({"command":command});
    let block = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("quiet-bash".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        "exit=0 duration=0.2s\n(no output)".into(),
        true,
        false,
        None,
        None,
    )));
    let rendered = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        42,
        false,
    );
    assert!(
        rendered[0].contains(&theme.settled_event_dot("success", "•")),
        "successful Bash dot should use the settled green tone: {rendered:?}"
    );
    let rendered = rendered
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();

    assert!(rendered[0].starts_with("• Bash"), "{rendered:?}");
    let command_byte = rendered[0].find("node").expect("command on Bash row");
    let command_column = visible_width(&rendered[0][..command_byte]);
    assert_eq!(
        command_column, 8,
        "Bash input should begin two cells after its label: {rendered:?}"
    );
    let output_row = rendered
        .iter()
        .position(|line| line.contains("(no output)"))
        .expect("no-output row index");
    let no_output = rendered
        .iter()
        .find(|line| line.contains("(no output)"))
        .expect("no-output metadata");
    let elbow_byte = no_output.find('└').expect("nested output elbow");
    let elbow_column = visible_width(&no_output[..elbow_byte]);
    let output_byte = no_output.find("(no output)").expect("nested output text");
    let output_column = visible_width(&no_output[..output_byte]);
    assert_eq!(elbow_column, 2, "{rendered:?}");
    assert_eq!(
        output_column,
        elbow_column + 2,
        "Bash metadata must begin one level after the elbow: {rendered:?}"
    );
    let wrapped_headers = &rendered[1..output_row];
    assert!(
        !wrapped_headers.is_empty(),
        "fixture must wrap the Bash command: {rendered:?}"
    );
    for continuation in wrapped_headers {
        let stem_byte = continuation
            .find('│')
            .expect("wrapped tool headers need a vertical output stem");
        assert_eq!(
            visible_width(&continuation[..stem_byte]),
            elbow_column,
            "tool stem and output elbow must share the tool-label column: {rendered:?}"
        );
        assert!(
            visible_width(continuation) > command_column,
            "wrapped commands must retain their label-relative value column: {rendered:?}"
        );
    }
    assert_eq!(
        rendered.iter().filter(|line| line.contains('└')).count(),
        1,
        "tool output needs one connector for the whole nested group: {rendered:?}"
    );
    assert!(
        rendered
            .iter()
            .all(|line| !line.chars().any(|character| matches!(character, '✓' | '×'))),
        "the margin dot is the only lifecycle marker: {rendered:?}"
    );
}

#[test]
fn bash_output_and_hidden_metadata_share_a_terminal_content_gutter() {
    let theme = crate::tui::theme::test_theme_for(
        TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    );
    let command = "printf result";
    let args = serde_json::json!({"command": command});
    let output = (1..=8)
        .map(|line| format!("result line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let panel = ToolPanel::new(
        ToolCallId("bash-gutter".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        format!("exit=0 duration=0.2s\nstdout: 8 lines\n{output}"),
        true,
        false,
        None,
        None,
    );

    let details = render_compact_bash_output(&panel, &theme, 80, false, &tool_value_indent("Bash"));
    assert!(
        details[2].contains("\x1b[38;2;"),
        "hidden-line metadata should use the muted metadata style: {details:?}"
    );
    assert!(
        details[1].contains("\x1b[38;2;"),
        "raw Bash output should use the readable output tint: {details:?}"
    );

    let block = TranscriptBlock::Tool(Box::new(panel));
    let rendered = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        false,
    )
    .into_iter()
    .map(|line| strip_terminal_sequences(&line))
    .collect::<Vec<_>>();
    let label_byte = rendered[0].find("Bash").expect("label on Bash row");
    let label_column = visible_width(&rendered[0][..label_byte]);
    let hidden = rendered
        .iter()
        .find(|line| line.contains("4 output rows collapsed"))
        .expect("synthetic hidden-line metadata");
    let output = rendered
        .iter()
        .find(|line| line.contains("result line 1"))
        .expect("first retained output row");
    let elbow_byte = output.find('└').expect("nested output elbow");
    let elbow_column = visible_width(&output[..elbow_byte]);
    let hidden_byte = hidden.find('…').expect("hidden metadata marker");
    let hidden_column = visible_width(&hidden[..hidden_byte]);
    let output_byte = output.find("result line 1").expect("retained output text");
    let output_column = visible_width(&output[..output_byte]);
    assert_eq!(elbow_column, label_column, "{rendered:?}");
    assert_eq!(hidden_column, elbow_column + 2, "{rendered:?}");
    assert_eq!(output_column, elbow_column + 2, "{rendered:?}");
    let TranscriptBlock::Tool(panel) = &block else {
        unreachable!("fixture is a Bash tool panel");
    };
    assert!(
        !panel.output.contains("lines hidden"),
        "synthetic UI metadata must not enter the raw tool payload"
    );
}

#[test]
fn compact_bash_window_is_capped_at_five_physical_rows_after_wrapping() {
    let theme = crate::tui::theme::test_theme();
    let args = serde_json::json!({"command": "printf wrapped"});
    let panel = ToolPanel::new(
        ToolCallId("bash-physical-window".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        format!(
            "exit=0 duration=0.2s\nstdout: 4 lines\n\x1b[31m{}\x1b[0m\n{}\n{}\npartial-tail",
            "界".repeat(30),
            "wrapped-ascii-".repeat(12),
            "e\u{301}".repeat(50),
        ),
        true,
        false,
        None,
        None,
    );

    for width in [18, 24, 42, 80] {
        let rows =
            render_compact_bash_output(&panel, &theme, width, false, &tool_value_indent("Bash"));
        assert_eq!(
            rows.len(),
            COMPACT_EXEC_OUTPUT_ROWS,
            "width {width}: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|row| visible_width(row) <= usize::from(width)),
            "width {width}: {rows:?}"
        );
    }
}

#[test]
fn carriage_return_bash_progress_replaces_one_visual_row() {
    let theme = crate::tui::theme::test_theme();
    let args = serde_json::json!({"command": "progress"});
    let panel = ToolPanel::new(
        ToolCallId("bash-cr-progress".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        "phase\nprogress 0\rprogress 10\rprogress 100".into(),
        false,
        false,
        None,
        None,
    );
    let rows = render_compact_bash_output(&panel, &theme, 80, false, &tool_value_indent("Bash"));
    let plain = rows
        .iter()
        .map(|row| strip_terminal_sequences(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        rows.len(),
        2,
        "short progress output must not reserve blank rows"
    );
    assert!(plain.contains("progress 100"), "{plain}");
    assert!(!plain.contains("progress 10\n"), "{plain}");
    assert!(!plain.contains("progress 0\n"), "{plain}");
}

#[test]
fn adjacent_bash_cards_do_not_reserve_blank_output_rows() {
    let shell = InteractiveShell::test_shell();
    let args = serde_json::json!({"command": "first"});
    let second_args = serde_json::json!({"command": "second"});
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("compact-first".into()),
            "bash".into(),
            args.to_string(),
            summarize_tool("bash", &args),
            String::new(),
            false,
            false,
            None,
            None,
        ))));
        state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("compact-second".into()),
            "bash".into(),
            second_args.to_string(),
            summarize_tool("bash", &second_args),
            String::new(),
            false,
            false,
            None,
            None,
        ))));
    }

    let card_height = |state: &ShellState| {
        let _ = state.rendered_transcript(32);
        let cache = state.transcript_cache.borrow();
        cache.block_starts[1].saturating_sub(cache.block_starts[0])
    };
    let waiting_height = card_height(&shell.state.borrow());

    let mut heights = Vec::new();
    for (index, output) in [
        "one partial line".to_owned(),
        format!("{}\nlast", "界 wrapped output ".repeat(20)),
        "exit=0 duration=0.1s\nstdout: 2 lines\nfinal one\nfinal two".to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        let mut state = shell.state.borrow_mut();
        let TranscriptBlock::Tool(panel) = &mut state.transcript[0] else {
            unreachable!()
        };
        panel.output = output;
        panel.finished = index == 2;
        state.touch_block(0);
        heights.push(card_height(&state));
    }

    assert!(
        heights[0] <= waiting_height,
        "one output row should replace the waiting row without reserving blank space: waiting={waiting_height}, rendered={heights:?}"
    );
    assert!(
        heights[1] > heights[0],
        "a full five-row tail may grow the card"
    );
    assert!(
        heights[1] < heights[0] + COMPACT_EXEC_OUTPUT_ROWS,
        "collapsed output exceeded its five-row budget: {heights:?}"
    );
    assert!(
        heights[2] < heights[1],
        "a short final result should release unused output rows: {heights:?}"
    );
}

#[test]
fn final_tool_result_replaces_live_output_without_the_tui_byte_cap() {
    use octet_agent::ToolOutput;

    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("local");
    let id = ToolCallId("bash-final-replaces-live".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "bash".into(),
            args: serde_json::json!({"command": "large-final"}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolProgress {
            id: id.clone(),
            progress: ToolProgress::Output {
                stream: octet_agent::OutputStream::Stdout,
                bytes: bytes::Bytes::from_static(b"LIVE-ONLY-SENTINEL"),
            },
        },
    );
    let final_output = format!(
        "exit=0 duration=0.1s\nstdout: 1 lines\n{}FINAL-SENTINEL",
        "x".repeat(70 * 1024)
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: id.clone(),
            result: Ok(ToolOutput::new(final_output.clone())),
            duration: Duration::from_millis(10),
        },
    );

    let retained = shell.debug_tool_output(&id).expect("retained final output");
    assert_eq!(retained, final_output);
    assert!(!retained.contains("LIVE-ONLY-SENTINEL"));
    assert!(retained.ends_with("FINAL-SENTINEL"));
}

#[test]
fn failed_bash_output_keeps_a_bounded_excerpt_before_expansion() {
    let theme = crate::tui::theme::test_theme();
    let args = serde_json::json!({"command": "failing-command"});
    let output = (1..=8)
        .map(|line| format!("failed output line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let block = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("failed-bash-output".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        format!("error nonzero_exit\nexit=1 duration=0.2s\nstdout: 8 lines\n{output}"),
        true,
        true,
        Some("command exited with code 1".into()),
        None,
    )));

    let collapsed = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        false,
    )
    .into_iter()
    .map(|line| strip_terminal_sequences(&line))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(collapsed.contains("failed output line 1"), "{collapsed}");
    assert!(!collapsed.contains("failed output line 4"), "{collapsed}");
    assert!(collapsed.contains("failed output line 8"), "{collapsed}");
    assert!(collapsed.contains("output rows collapsed"), "{collapsed}");

    let expanded = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        true,
    )
    .into_iter()
    .map(|line| strip_terminal_sequences(&line))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(expanded.contains("failed output line 1"), "{expanded}");
    assert!(expanded.contains("failed output line 8"), "{expanded}");
}

#[test]
fn footer_collapses_semantically_and_keeps_one_adjacent_row() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(PathBuf::from("/work/octet-footer-regression"));
    shell.set_identity(
        "custom-openai",
        "custom/unsloth/Qwen3.6-35B-A3B-MTP-GGUF",
        "high",
    );
    {
        let mut state = shell.state.borrow_mut();
        state.last_turn_usage = Some(Usage {
            input_tokens: 26_800,
            output_tokens: 422,
            total_tokens: 27_222,
            ..Usage::default()
        });
        state.last_turn_tokens_per_second = Some(41.9);
        state.context_estimate = Some((5_600, 246_000));
        state.price_display = PriceDisplay::ExplicitZero;
        state.telemetry_model = Some(state.model.clone());
    }
    let now = Instant::now();
    let wide = plain_footer(&shell, 100, now);
    assert!(wide.starts_with("  Qwen3.6 35B A3B · high"), "{wide:?}");
    assert!(wide.contains("2%/246K"), "{wide:?}");
    assert!(wide.contains(" · 2%/246K · $0"), "{wide:?}");
    assert!(wide.ends_with("/work/octet-footer-regression"), "{wide:?}");
    assert_eq!(visible_width(&wide), 98);
    assert!(!wide.contains('↑') && !wide.contains('↓'), "{wide:?}");

    let medium = plain_footer(&shell, 68, now);
    assert!(medium.contains("Qwen3.6 35B A3B · high"), "{medium:?}");
    assert!(medium.contains("2%/246K"), "{medium:?}");
    assert!(medium.contains(" · 2%/246K · $0"), "{medium:?}");
    assert!(
        medium.contains('…') && medium.ends_with("footer-regression"),
        "{medium:?}"
    );
    assert_eq!(visible_width(&medium), 66);

    let compact = plain_footer(&shell, 44, now);
    assert!(compact.contains("Qwen3.6 35B A3B"), "{compact:?}");
    assert!(compact.contains("2%"), "{compact:?}");
    assert!(compact.ends_with(" · 2%/246K · $0"), "{compact:?}");
    assert!(compact.contains("high"), "{compact:?}");
    assert!(!compact.contains("footer-regression"), "{compact:?}");

    let narrow = plain_footer(&shell, 30, now);
    assert!(narrow.contains("Qwen3.6 35B A3B"), "{narrow:?}");
    assert!(narrow.contains("2%"), "{narrow:?}");
    assert!(!narrow.contains("session"), "{narrow:?}");

    let surface = plain_composer_surface(&shell, 100, now);
    assert_eq!(surface.len(), 4, "one editor row, two rules, one footer");
    assert!(!surface[surface.len() - 2].is_empty());
    assert_eq!(surface.last().unwrap(), &plain_footer(&shell, 100, now));
    assert!(surface.iter().all(|line| visible_width(line) <= 100));
}

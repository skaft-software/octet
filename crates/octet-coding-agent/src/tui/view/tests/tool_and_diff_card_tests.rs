//! Tool cards, edit and write diffs, tool labels, narrow-width degradation, and the rich
//! markdown that renders inside an assistant answer. Separate because they all assert how much
//! text a tool result or an answer is allowed to put on screen.

use super::support::*;

use super::*;

#[test]
fn renderer_covers_idle_and_every_active_run_phase() {
    let mut idle = InteractiveShell::test_shell();
    idle.set_identity("relay", "gpt-5.6", "high");
    let idle = render_shell(&idle.state.borrow(), 80).join("\n");
    assert!(idle.contains("GPT-5.6"), "{idle}");
    assert!(!idle.contains("relay / "));
    // No newline shortcut hint is shown in idle footer

    let cases = [
        RunPhase::AwaitingProvider {
            provider: "relay".into(),
        },
        RunPhase::Thinking,
        RunPhase::StreamingResponse,
        RunPhase::PreparingToolCall,
        RunPhase::RunningTool {
            summary: "running tests".into(),
        },
        RunPhase::AwaitingApproval {
            prompt: "allow edit".into(),
        },
        RunPhase::Preparing {
            summary: "compacting".into(),
        },
    ];
    for phase in cases {
        let rendered = rendered_phase(phase);
        assert!(rendered.contains("GPT-5.6"), "{rendered}");
        assert!(!rendered.contains("Working"), "{rendered}");
    }
}

#[test]
fn custom_theme_keeps_active_work_out_of_the_footer() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_theme(crate::tui::theme::test_theme_from_source(
        SURFACE_TEST_THEME,
    ));
    let now = Instant::now();
    {
        let mut state = shell.state.borrow_mut();
        let id = state.run.begin_at("relay", now).unwrap();
        state.run.set_phase_at(id, RunPhase::Thinking, now);
    }
    let rendered =
        render_shell_at(&shell.state.borrow(), 80, now + Duration::from_millis(600)).join("\n");
    assert!(!rendered.contains("Working"), "{rendered}");
    assert!(!rendered.contains("0.6s"), "{rendered}");
}

#[test]
fn default_footer_accumulates_work_but_never_shows_the_stopwatch() {
    let shell = InteractiveShell::test_shell();
    let now = Instant::now();
    {
        let mut state = shell.state.borrow_mut();
        let first = state
            .run
            .begin_at("relay", now - Duration::from_secs(4))
            .unwrap();
        let outcome = state.run.interrupt_at(first, now).unwrap();
        InteractiveShell::append_outcome(&mut state, outcome);
        assert_eq!(state.session_work_elapsed, Duration::from_secs(4));

        let second = state.run.begin_at("relay", now).unwrap();
        state.run.set_phase_at(second, RunPhase::Thinking, now);
    }

    let active = strip_terminal_sequences(
        &crate::tui::composer_surface::render_composer_surface(
            &shell.state.borrow(),
            80,
            now + Duration::from_secs(2),
        )
        .join("\n"),
    );
    assert!(!active.contains("6.0s"), "{active}");

    {
        let mut state = shell.state.borrow_mut();
        let second = state.run.current_id().unwrap();
        let outcome = state
            .run
            .interrupt_at(second, now + Duration::from_secs(2))
            .unwrap();
        InteractiveShell::append_outcome(&mut state, outcome);
        assert_eq!(state.session_work_elapsed, Duration::from_secs(6));
    }
    let idle_later = strip_terminal_sequences(
        &crate::tui::composer_surface::render_composer_surface(
            &shell.state.borrow(),
            80,
            now + Duration::from_secs(32),
        )
        .join("\n"),
    );
    assert!(!idle_later.contains("6.0s"), "{idle_later}");
    assert!(!idle_later.contains("36.0s"), "{idle_later}");
}

#[test]
fn ctrl_o_keeps_width_cache_and_invalidates_only_disclosure_blocks() {
    let mut shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        for index in 0..256 {
            state.push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(format!("stable answer {index}")),
            )));
        }
        state.push_block(TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::finalized_reasoning("expand me".into()),
        )));
        let _ = state.rendered_transcript(100);
        assert_eq!(state.transcript_cache.borrow().width, Some(100));
    }

    shell.toggle_disclosure();
    let state = shell.state.borrow();
    let cache = state.transcript_cache.borrow();
    assert_eq!(cache.width, Some(100));
    assert_eq!(cache.dirty_blocks, [256]);
}

#[test]
fn tool_output_uses_one_compact_nested_elbow() {
    let theme = crate::tui::theme::test_theme();
    let renderer = theme.rich_renderer();
    let args = serde_json::json!({"command": "printf hello"});
    let block = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("aligned-tool-output".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        "exit=0 duration=0.2s\nstdout:\nhello\ncomplete_stdout=true".into(),
        true,
        false,
        None,
        None,
    )));
    let lines = render_block(None, &block, &theme, &renderer, &renderer, 80, false)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    let output = lines
        .iter()
        .find(|line| line.contains("└ hello"))
        .expect("tool output should render");
    let command = lines
        .iter()
        .find(|line| line.contains("Bash  printf hello"))
        .expect("tool input should render");
    let label_column = command
        .find("Bash")
        .map(|index| visible_width(&command[..index]))
        .expect("tool label should render");
    let elbow_column = output
        .find('└')
        .map(|index| visible_width(&output[..index]))
        .expect("tool output elbow should render");
    let output_column = output
        .find("hello")
        .map(|index| visible_width(&output[..index]))
        .expect("tool output value should render");
    assert_eq!(
        label_column, 2,
        "tool labels belong on the primary text column"
    );
    assert_eq!(elbow_column, label_column, "{lines:?}");
    assert_eq!(output_column, elbow_column + 2, "{lines:?}");
    assert_eq!(
        lines.iter().filter(|line| line.contains('└')).count(),
        1,
        "one tool output group needs exactly one elbow: {lines:?}"
    );
}

#[test]
fn transcript_events_prompt_and_composer_share_one_grid() {
    let theme = crate::tui::theme::test_theme();
    let renderer = theme.rich_renderer();
    let prompt = TranscriptBlock::User {
        text: "prompt".into(),
        model_lab: Some(ModelLab::OpenAi),
        prompt_color: Some("#123456".into()),
        persisted: true,
    };
    let assistant =
        TranscriptBlock::Assistant(Box::new(AssistantBlock::finalized("answer".into())));
    let mut working =
        AssistantBlock::streaming_reasoning("").with_model_lab(Some(ModelLab::OpenAi));
    working.reasoning_heading = Some("Working".into());
    working.show_reasoning_hint = false;
    let working = TranscriptBlock::Reasoning(Box::new(working));
    let args = serde_json::json!({"command": "printf hello"});
    let tool = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("shared-grid".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        "exit=0 duration=0.2s\nstdout:\nhello\ncomplete_stdout=true".into(),
        true,
        false,
        None,
        None,
    )));

    let rendered_row = |block: &TranscriptBlock, needle: &str| {
        render_block(None, block, &theme, &renderer, &renderer, 80, false)
            .into_iter()
            .map(|line| strip_terminal_sequences(&line))
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} transcript row"))
    };
    let prompt_row = rendered_row(&prompt, "prompt");
    let assistant_row = rendered_row(&assistant, "answer");
    let working_row = rendered_row(&working, "Working");
    let tool_row = rendered_row(&tool, "Bash");
    let shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().editor.set_text("draft");
    let composer_row = plain_composer_surface(&shell, 80, Instant::now())
        .into_iter()
        .find(|line| line.contains("draft"))
        .expect("composer draft row");
    let column = |line: &str, needle: &str| {
        let byte = line
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?}: {line:?}"));
        visible_width(&line[..byte])
    };

    let marker_column = column(&prompt_row, "›");
    assert_eq!(marker_column, 0, "prompt marker must own column zero");
    assert_eq!(
        column(&composer_row, "›"),
        marker_column,
        "{composer_row:?}"
    );
    assert_eq!(
        column(&assistant_row, "•"),
        marker_column,
        "{assistant_row:?}"
    );
    assert_eq!(column(&working_row, "•"), marker_column, "{working_row:?}");
    assert_eq!(column(&tool_row, "•"), marker_column, "{tool_row:?}");

    let text_column = column(&prompt_row, "prompt");
    assert_eq!(text_column, 2, "primary text must begin at column two");
    assert_eq!(
        column(&composer_row, "draft"),
        text_column,
        "{composer_row:?}"
    );
    assert_eq!(
        column(&assistant_row, "answer"),
        text_column,
        "{assistant_row:?}"
    );
    assert_eq!(
        column(&working_row, "Working"),
        text_column,
        "{working_row:?}"
    );
    assert_eq!(column(&tool_row, "Bash"), text_column, "{tool_row:?}");
}

#[test]
fn tool_rendering_shows_bounded_failure_evidence_but_hides_transport_metadata() {
    use octet_agent::{ToolError, ToolOutput};
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    let id = ToolCallId("provider-call-secret".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "bash".into(),
            args: serde_json::json!({"command": "cargo test --workspace", "timeout_ms": 1000}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id,
            result: Ok(ToolOutput::new(
                "exit=1 duration=0.2s\nstderr: FAILED 76 passed",
            )),
            duration: Duration::from_millis(200),
        },
    );
    let plain = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 80).join("\n"));
    assert!(plain.contains("Bash  cargo test --workspace"), "{plain:?}");
    assert!(!plain.contains("provider-call-secret"), "{plain:?}");
    assert!(!plain.contains("exit=1"), "{plain:?}");
    assert!(!plain.contains("duration=0.2s"), "{plain:?}");
    assert!(plain.contains("stderr: FAILED 76 passed"), "{plain:?}");
    assert!(plain.contains("command exited 1"), "{plain:?}");
    let stale = ToolCallId("stale-edit-id".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: stale.clone(),
            name: "edit".into(),
            args: serde_json::json!({"path":"src/lib.rs"}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: stale,
            result: Err(ToolError::new(
                "error stale_file\nexpected hash=aaa actual=bbb\nThe file changed",
            )),
            duration: Duration::from_millis(10),
        },
    );
    let plain = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 120).join("\n"));
    // The finished edit keeps its intent; wall time is reserved for bash.
    assert!(plain.contains("Edit"), "{plain:?}");
    assert!(plain.contains("src/lib.rs"), "{plain:?}");
    assert!(!plain.contains("· 10 ms"), "{plain:?}");
    assert!(plain.contains("The file changed"), "{plain:?}");
    assert!(!plain.contains("hash=aaa"), "{plain:?}");
    assert!(!plain.contains("actual=bbb"), "{plain:?}");
}

#[test]
fn successful_media_reads_render_payload_free_capability_indicators() {
    use octet_agent::{ToolError, ToolOutput};

    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("openai");
    let image_id = ToolCallId("image-read".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: image_id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path": "capture.png"}),
        },
    );
    let image_output = ToolOutput::new("image summary")
        .with_media(octet_ai::Media::image_bytes(
            bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\n"),
            mime::IMAGE_PNG,
        ))
        .without_media_payloads();
    assert!(image_output.media().is_empty());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: image_id,
            result: Ok(image_output),
            duration: Duration::from_millis(10),
        },
    );

    for (id, path, result) in [
        (
            "text-read",
            "notes.txt",
            Ok(ToolOutput::new("plain text summary")),
        ),
        (
            "failed-read",
            "broken.png",
            Err(ToolError::new("unsupported image encoding")),
        ),
    ] {
        let id = ToolCallId(id.into());
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: "read".into(),
                args: serde_json::json!({"path": path}),
            },
        );
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolFinished {
                id,
                result,
                duration: Duration::from_millis(10),
            },
        );
    }

    let rendered = shell
        .state
        .borrow()
        .rendered_transcript(100)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    let image = rendered
        .iter()
        .find(|line| line.contains("capture.png"))
        .expect("successful image read row");
    assert!(image.contains('◉'), "{rendered:?}");
    assert!(!image.contains('♪'), "{rendered:?}");
    for path in ["notes.txt", "broken.png"] {
        let line = rendered
            .iter()
            .find(|line| line.contains(path))
            .expect("non-media read row");
        assert!(
            !line.contains('◉') && !line.contains('♪'),
            "unsupported or failed reads must not imply media ingestion: {rendered:?}"
        );
    }
}

#[test]
fn responsive_header_drops_metadata_instead_of_truncating_every_field() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("relay", "gpt-5.6", "high");
    let wide = responsive_identity(&shell.state.borrow(), 120);
    assert!(wide.contains("relay / "));
    assert!(wide.contains("GPT-5.6"));
    assert!(wide.contains("high"));

    shell.set_identity(
        "custom-openai",
        "custom/Intel/Qwen3.6-27B-int4-AutoRound",
        "high",
    );
    let custom = strip_terminal_sequences(&responsive_identity(&shell.state.borrow(), 120));
    assert!(custom.contains("custom-openai / Qwen3.6 27B"), "{custom}");
    assert!(!custom.contains("custom/Intel"), "{custom}");

    shell.set_identity(
        "a-very-long-gateway-provider-name",
        "a-very-long-model-name-that-does-not-fit",
        "high",
    );
    let narrow = responsive_identity(&shell.state.borrow(), 40);
    assert!(visible_width(&narrow) <= 40);
    assert!(!narrow.contains("..."));
    assert!(!narrow.contains('…'));
    assert!(narrow.contains("octet"));
}

#[test]
fn ascii_plain_and_unicode_no_colour_keep_the_same_structure() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

    let ascii_theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
        false,
        false,
        ColorDepth::None,
    ));
    let mut ascii = InteractiveShell::test_shell_with_theme(ascii_theme);
    ascii.set_identity("relay", "gpt-5.6", "off");
    ascii.on_prompt_submitted("fix it");
    {
        let mut state = ascii.state.borrow_mut();
        state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized("# Result\n\n- done".into()),
        )));
        state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("id".into()),
            "edit".into(),
            "{}".into(),
            summarize_tool("edit", &serde_json::json!({"path":"src/lib.rs"})),
            String::new(),
            true,
            false,
            None,
            None,
        ))));
        state.push_block(TranscriptBlock::Outcome(OutcomeBlock::new(
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
    }
    ascii.set_size(40, 20);
    let ascii = render_shell(&ascii.state.borrow(), 40)
        .join("\n")
        .replace(CURSOR_MARKER, "");
    assert!(ascii.is_ascii(), "{ascii:?}");
    assert!(!ascii.contains('\x1b'));
    assert!(ascii.contains("> fix it"));
    assert!(ascii.contains("Result"));
    assert!(ascii.contains("- done"));
    assert!(ascii.contains("Edit"));
    assert!(ascii.contains("lib.rs"));
    assert!(ascii.contains("completed - 1.0s"));
    assert!(!ascii.contains("ok completed"));

    let unicode_theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
        true,
        true,
        ColorDepth::None,
    ));
    let mut unicode = InteractiveShell::test_shell_with_theme(unicode_theme);
    unicode.on_prompt_submitted("fix it");
    let unicode = render_shell(&unicode.state.borrow(), 60)
        .join("\n")
        .replace(CURSOR_MARKER, "");
    assert!(unicode.contains("› fix it"));
    assert!(!unicode.contains('\x1b'));
}

#[test]
fn narrow_tool_paths_use_basenames_and_wide_paths_remain_inspectable() {
    let theme = crate::tui::theme::test_theme();
    let panel = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("id".into()),
        "edit".into(),
        serde_json::json!({"path":"crates/octet-agent/src/session.rs"}).to_string(),
        summarize_tool(
            "edit",
            &serde_json::json!({"path":"crates/octet-agent/src/session.rs"}),
        ),
        String::new(),
        true,
        false,
        None,
        None,
    )));
    let renderer = theme.rich_renderer();
    let narrow = strip_terminal_sequences(
        &render_block(None, &panel, &theme, &renderer, &renderer, 40, false).join("\n"),
    );
    let wide = strip_terminal_sequences(
        &render_block(None, &panel, &theme, &renderer, &renderer, 120, false).join("\n"),
    );
    assert!(narrow.contains("Edit  session.rs"));
    assert!(!narrow.contains("crates/octet-agent"));
    assert!(wide.contains("Edit  crates/octet-agent/src/session.rs"));
}

#[test]
fn edit_status_prefix_does_not_hide_the_unified_diff() {
    let theme = crate::tui::theme::test_theme();
    let panel = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("edit-diff".into()),
        "edit".into(),
        "{}".into(),
        summarize_tool("edit", &serde_json::json!({"path":"src/lib.rs"})),
        concat!(
            "ok modified=1\n",
            "src/lib.rs  +1 -1 hash=abc\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,1 +1,1 @@\n",
            "-old\n",
            "+new\n"
        )
        .into(),
        true,
        false,
        None,
        None,
    )));
    let renderer = theme.rich_renderer();
    let rendered = render_block(None, &panel, &theme, &renderer, &renderer, 100, false).join("\n");
    let plain = strip_terminal_sequences(&rendered);
    assert!(plain.contains("-old"), "{rendered}");
    assert!(plain.contains("+new"), "{rendered}");
    assert!(!plain.contains("hash=abc"), "{rendered}");
}

#[test]
fn compact_edit_keeps_both_replacement_sides_with_long_paths() {
    let theme = crate::tui::theme::test_theme();
    let path = format!("/work/{}/pagination.py", "long-directory/".repeat(15));
    let args = serde_json::json!({"path": path});
    let panel = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("compact-replacement".into()),
        "edit".into(),
        args.to_string(),
        summarize_tool("edit", &args),
        format!(
            "--- a/{path}\n+++ b/{path}\n@@ -1,5 +1,5 @@\n context\n context\n-start = page * size\n+start = (page - 1) * size\n context\n context\n"
        ),
        true,
        false,
        None,
        None,
    )));
    let renderer = theme.rich_renderer();
    for width in [60, 80, 120] {
        let rows = render_block(None, &panel, &theme, &renderer, &renderer, width, false);
        let plain = strip_terminal_sequences(&rows.join("\n"));
        assert!(plain.contains("-start = page * size"), "{width}: {plain}");
        assert!(
            plain.contains("+start = (page - 1) * size"),
            "{width}: {plain}"
        );
        assert!(rows
            .iter()
            .all(|row| visible_width(row) <= usize::from(width)));
    }
}

#[test]
fn layered_write_diff_reports_one_truthful_remainder_per_disclosure_mode() {
    let theme = crate::tui::theme::test_theme();
    let renderer = theme.rich_renderer();
    let args = serde_json::json!({"path":"large.txt"});
    let preview = (1..=10)
        .map(|line| format!("+line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let block = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("layered-write-diff".into()),
        "write".into(),
        args.to_string(),
        summarize_tool("write", &args),
        format!(
            "ok\nlarge.txt  created hash=abc\n--- /dev/null\n+++ b/large.txt\n@@ -0,0 +1,191 @@\n{preview}\n… 181 more lines\n"
        ),
        true,
        false,
        None,
        None,
    )));

    let collapsed = strip_terminal_sequences(
        &render_block(None, &block, &theme, &renderer, &renderer, 100, false).join("\n"),
    );
    assert!(collapsed.contains("182 lines hidden"), "{collapsed:?}");
    assert!(!collapsed.contains("181 more lines"), "{collapsed:?}");
    assert_eq!(
        collapsed.matches("lines hidden").count(),
        1,
        "{collapsed:?}"
    );

    let expanded = strip_terminal_sequences(
        &render_block(None, &block, &theme, &renderer, &renderer, 100, true).join("\n"),
    );
    assert!(expanded.contains("@@ -0,0 +1,191 @@"), "{expanded:?}");
    assert!(expanded.contains("+line-10"), "{expanded:?}");
    assert_eq!(
        expanded.matches("181 more lines").count(),
        1,
        "{expanded:?}"
    );
    assert!(!expanded.contains("lines hidden"), "{expanded:?}");
}

#[test]
fn tool_values_follow_labels_without_a_wide_dead_column() {
    for (label, expected_column) in [
        ("Read", 6),
        ("Bash", 6),
        ("Write", 7),
        ("Explored", 10),
        ("Delegated", 11),
    ] {
        assert_eq!(tool_value_indent_width(label), expected_column, "{label}");
        assert_eq!(
            visible_width(&tool_value_indent(label)),
            expected_column,
            "{label}"
        );
        assert_eq!(
            expected_column.saturating_sub(visible_width(label)),
            2,
            "{label}"
        );
    }
    assert!(visible_width(&tool_grid_label("an_extremely_long_tool_name")) <= 18);
}

#[test]
fn computer_use_labels_keep_distinct_actions_within_the_width_cap() {
    let names = [
        "computer_use_status",
        "computer_use_setup",
        "computer_use_installed_apps",
        "computer_use_windows",
        "computer_use_window_state",
        "computer_use_desktop_state",
        "computer_use_click",
        "computer_use_type_text",
        "computer_use_press_key",
        "computer_use_hotkey",
        "computer_use_invoke_menu",
        "computer_use_move_cursor",
        "computer_use_scroll",
        "computer_use_launch_app",
        "computer_use_start_session",
        "computer_use_end_session",
        "computer_use_jev_status",
        "computer_use_jev_choose",
        "computer_use_jev_use_status",
        "computer_use_jev_use_cancel",
        "computer_use_jev_use_setup",
        "computer_use_jev_use_run",
        "computer_use_jev_use_choose",
    ];
    let labels: HashSet<_> = names
        .iter()
        .map(|name| {
            let label = super::tool_render::tool_display_label(name);
            assert_eq!(tool_grid_label(&label), label, "{name}");
            label
        })
        .collect();
    assert_eq!(labels.len(), names.len());
    assert_eq!(
        super::tool_render::tool_display_label("computer_use_window_state"),
        "Window state"
    );
}

#[test]
fn recognized_assistant_diffs_use_the_pretty_diff_renderer() {
    let theme = crate::tui::theme::test_theme();
    let assistant = AssistantBlock::finalized(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new".into(),
        );
    let rendered_lines = assistant.render(&theme.rich_renderer(), &theme, 80);
    let rendered = rendered_lines.join("\n");
    let plain = strip_terminal_sequences(&rendered);
    assert!(plain.contains("@@ -1 +1 @@"));
    assert!(plain.contains("-old"));
    assert!(plain.contains("+new"));
    assert!(!plain.contains("```"));

    let terminal = emulate_rows(&rendered_lines, 80);
    let (removed_row, removed_col) =
        find_ascii_cell(terminal.screen(), "-old").expect("rendered removal");
    let (added_row, added_col) =
        find_ascii_cell(terminal.screen(), "+new").expect("rendered addition");
    let removed = terminal
        .screen()
        .cell(removed_row, removed_col)
        .expect("removal cell")
        .fgcolor();
    let added = terminal
        .screen()
        .cell(added_row, added_col)
        .expect("addition cell")
        .fgcolor();
    assert_ne!(removed, vt100::Color::Default);
    assert_ne!(added, vt100::Color::Default);
    assert_ne!(removed, added);
}

#[test]
fn fenced_diff_inside_markdown_does_not_hijack_the_whole_answer() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

    let markdown = concat!(
        "## Why this changed\n\n",
        "The cache remains authoritative.\n\n",
        "```diff\n",
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "```\n",
    );
    assert!(!looks_like_diff(markdown));
    assert!(looks_like_diff(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new"
    ));
    assert!(looks_like_diff(
        "--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1,1 @@\n+fn main() {}"
    ));

    let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
        true,
        true,
        ColorDepth::None,
    ));
    let rendered = AssistantBlock::finalized(markdown.to_owned())
        .render(&theme.rich_renderer(), &theme, 60)
        .join("\n");
    assert!(rendered.contains("Why this changed"), "{rendered}");
    assert!(
        rendered.contains("cache remains authoritative"),
        "{rendered}"
    );
    assert!(rendered.contains("-old"), "{rendered}");
    assert!(!rendered.contains("```"), "{rendered}");
}

#[test]
fn assistant_markdown_uses_full_rich_pipeline_without_rewriting_source() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};

    let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
        true,
        true,
        ColorDepth::None,
    ));
    let source = concat!(
        "# Result\n\n",
        "> Safe presentation projection\n\n",
        "- [x] CommonMark\n",
        "- [ ] cached source\n\n",
        "| Feature | State |\n| --- | --- |\n| tables | on |\n\n",
        "See [the docs](https://example.com/octet).\n\n",
        "```rust\nlet complete_value = 12345678901234567890;\n```",
    );
    let assistant = AssistantBlock::finalized(source.to_owned());
    let renderer = theme.rich_renderer();
    let rendered = assistant.render(&renderer, &theme, 32).join("\n");

    // Rendering is a view over the exact provider/session payload. It may
    // add terminal structure, but it never normalizes the cached Markdown.
    assert_eq!(assistant.text, source);
    assert_eq!(assistant.markdown.raw_text(), source);
    assert_eq!(
        renderer.options().code_overflow,
        sexy_tui_rs::CodeOverflow::Wrap
    );
    assert!(renderer.options().syntax_highlighting);
    assert!(renderer.options().tables);
    assert!(!renderer.options().code_borders);

    assert!(rendered.contains("Result"), "{rendered}");
    assert!(
        rendered.contains("Safe presentation projection"),
        "{rendered}"
    );
    assert!(rendered.contains("CommonMark"), "{rendered}");
    assert!(rendered.contains("Feature"), "{rendered}");
    assert!(rendered.contains("tables"), "{rendered}");
    assert!(rendered.contains("https://example.com/octet"), "{rendered}");
    // The end of a long code row remains visible because transcript code
    // wraps instead of being irretrievably clipped.
    assert!(rendered.contains("67890"), "{rendered}");
    assert!(!rendered.contains("```"), "{rendered}");
    assert!(!rendered.contains("\x1b[48;"), "{rendered:?}");
}

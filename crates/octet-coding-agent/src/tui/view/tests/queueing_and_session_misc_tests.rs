//! Queued follow-ups across hydration, resume sorting, transcript link hit testing, and the
//! remaining narrow-width composer probes. Separate because they assert what survives a session
//! boundary.

use super::support::*;

use super::*;

#[test]
fn queued_follow_ups_stay_with_their_session_across_hydration() {
    let directory = tempfile::tempdir().unwrap();
    let first = Session::create(directory.path().join("first.jsonl")).unwrap();
    let second = Session::create(directory.path().join("second.jsonl")).unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.hydrate(&first).unwrap();
    shell.queue_follow_up(ComposedInput::from_text("first session only".into()));
    shell.settle_queued_follow_ups(true);
    shell.hydrate(&second).unwrap();
    assert_eq!(shell.queued_follow_up_len(), 0);
    assert!(shell.take_ready_follow_up().is_none());
    shell.queue_follow_up(ComposedInput::from_text("second session only".into()));
    shell.settle_queued_follow_ups(true);
    shell.hydrate(&first).unwrap();
    assert_eq!(shell.queued_follow_up_len(), 1);
    assert!(
        shell.take_ready_follow_up().is_none(),
        "another session's settlement cannot authorize dispatch"
    );
    shell.settle_queued_follow_ups(true);
    assert_eq!(
        shell.take_ready_follow_up().unwrap().transcript_text,
        "first session only"
    );
    shell.hydrate(&second).unwrap();
    shell.edit_queued_message();
    assert_eq!(
        shell.drain_composed().transcript_text,
        "second session only"
    );
}

#[test]
fn newest_joint_pending_input_wins_across_steering_and_follow_ups() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("fixture");
    let (_prepared, receipt) = octet_agent::PreparedSteering::new("first steering");
    shell.queue_retractable_steering(
        receipt,
        "first steering".into(),
        "first steering".into(),
        Vec::new(),
    );
    shell.queue_follow_up(ComposedInput::from_text("newer follow-up".into()));

    shell.edit_queued_message();
    assert_eq!(shell.pending(), "newer follow-up");
    assert_eq!(shell.state.borrow().steering_queue.len(), 1);

    shell.clear_editor();
    shell.edit_queued_message();
    assert_eq!(shell.pending(), "first steering");
    assert!(shell.state.borrow().steering_queue.is_empty());
}

#[test]
fn refused_steering_admission_restores_the_draft_without_a_fifo_entry() {
    let mut shell = InteractiveShell::test_shell();
    shell.restore_unqueued_steering("refused steering".into(), Vec::new());

    assert_eq!(shell.pending(), "refused steering");
    assert!(shell.state.borrow().steering_queue.is_empty());
    let recomposed = shell.drain_composed();
    assert!(matches!(
        recomposed.parts.as_slice(),
        [octet_agent::InputPart::Text(text)] if text == "refused steering"
    ));
}

#[test]
fn queued_follow_up_editing_preserves_payloads_and_never_overwrites_a_draft() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("fixture");
    shell.queue_follow_up(ComposedInput::from_text("oldest".into()));
    let large = "queued payload\n".repeat(20);
    shell.apply_edit(EditAction::Paste(large.clone()));
    let composed = shell.drain_composed();
    let display = composed.display_text.clone();
    shell.queue_follow_up(composed);
    shell.apply_edit(EditAction::Paste("local draft".into()));
    shell.edit_queued_message();
    assert_eq!(shell.pending(), "local draft");
    assert_eq!(shell.state.borrow().follow_up_queue.len(), 2);
    shell.clear_editor();
    shell.edit_queued_message();
    assert_eq!(shell.pending(), display);
    assert_eq!(shell.state.borrow().follow_up_queue.len(), 1);
    assert!(shell.take_ready_follow_up().is_none());
    let edited = shell.drain_composed();
    assert!(
        matches!(edited.parts.as_slice(), [octet_agent::InputPart::Text(text)] if text == &large)
    );
    shell.queue_follow_up(edited);
    shell.settle_queued_follow_ups(true);
    assert_eq!(
        shell.take_ready_follow_up().unwrap().transcript_text,
        "oldest"
    );
    assert!(
        shell.take_ready_follow_up().is_none(),
        "only one prompt per settled run"
    );
    shell.settle_queued_follow_ups(true);
    assert_eq!(shell.take_ready_follow_up().unwrap().transcript_text, large);
}

#[test]
fn failed_queued_submission_restores_its_payload_alongside_the_new_draft() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Paste("queued payload\n".repeat(20)));
    let queued = shell.drain_composed();
    shell.apply_edit(EditAction::Paste("new draft\n".repeat(20)));
    shell.restore_composed(queued);
    let restored = shell.drain_composed();
    assert!(
        matches!(restored.parts.as_slice(), [octet_agent::InputPart::Text(text)]
        if text == &format!("{}\n\n{}", "queued payload\n".repeat(20), "new draft\n".repeat(20)))
    );
}

#[test]
fn queued_follow_up_preview_is_bounded_and_control_safe() {
    let mut shell = InteractiveShell::test_shell();
    shell.queue_follow_up(ComposedInput::from_text(
        "first\n\x1b[3J hostile".repeat(100),
    ));
    shell.queue_follow_up(ComposedInput::from_text("second".into()));
    for width in [20, 40, 80] {
        let rows = input_overlays::render_pending_steering(&shell.state.borrow(), width, 10);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| visible_width(row) <= width as usize));
        assert!(rows.iter().all(|row| !row.contains("\x1b[3J")));
    }
}

#[test]
fn queued_follow_up_heading_advertises_the_platform_edit_hint() {
    let mut steering_shell = InteractiveShell::test_shell();
    steering_shell.queue_steering(&ComposedInput::from_text("steering only".into()));
    let steering_only =
        input_overlays::render_pending_steering(&steering_shell.state.borrow(), 80, 2);
    let heading = strip_terminal_sequences(&steering_only[0]);
    assert!(heading.contains("Steering"), "{heading:?}");
    assert!(
        !heading.contains("to edit"),
        "admitted steering cannot be recalled, so it must not advertise an edit \
         affordance: {heading:?}"
    );

    let mut shell = InteractiveShell::test_shell();
    shell.queue_follow_up(ComposedInput::from_text("local follow-up".into()));
    let with_follow_up = input_overlays::render_pending_steering(&shell.state.borrow(), 80, 2);
    let heading = strip_terminal_sequences(&with_follow_up[0]);
    // Windows and WSL use the Windows-flavoured key set, which binds Alt+Q.
    let wsl = cfg!(target_os = "linux")
        && (std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::env::var_os("WSL_INTEROP").is_some());
    let expected = if cfg!(target_os = "macos") {
        "option+↑ to edit"
    } else if cfg!(windows) || wsl {
        "alt+q to edit"
    } else {
        "alt+↑ to edit"
    };
    assert!(heading.contains(expected), "{heading:?}");
    // The hint rides on the heading row and stays inside the viewport budget.
    assert_eq!(with_follow_up.len(), 2);
    assert_eq!(
        strip_terminal_sequences(&with_follow_up[1]),
        "  └ local follow-up"
    );
    for width in [20, 40, 80] {
        let rows = input_overlays::render_pending_steering(&shell.state.borrow(), width, 10);
        assert!(rows.iter().all(|row| visible_width(row) <= width as usize));
    }
}

#[test]
fn resume_sort_hotkey_reaches_relevance_and_threaded_parent_before_child() {
    let mut parent = picker_session("parent", "parent", 1, 1);
    parent.workspace = None;
    let mut child = picker_session("child", "child", 1, 2);
    child.forked_from_session_id = Some("parent".into());
    let mut shell = InteractiveShell::test_shell();
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(vec![child, parent], None)),
    });
    for expected in [
        PickerSort::Name,
        PickerSort::Messages,
        PickerSort::Relevance,
        PickerSort::Threaded,
    ] {
        shell.panel_input(&panel_key_with_modifiers(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        let state = shell.state.borrow();
        let Some(Panel::SessionPicker { picker }) = state.panel.as_ref() else {
            panic!("session picker");
        };
        assert_eq!(picker.sort, expected);
        if expected == PickerSort::Threaded {
            assert_eq!(session_picker_ordering(picker), vec![1, 0]);
        }
    }
    let selected = shell.panel_input(&panel_key(crossterm::event::KeyCode::Enter));
    assert!(matches!(selected, Some((PanelResult::Select(ref id), _)) if id == "parent"));
}

#[test]
fn unpriced_session_usage_is_retained_by_live_telemetry_and_hydration_with_priced_model() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unpriced.jsonl");
    let mut session = Session::create(&path).unwrap();
    let usage = octet_ai::Usage {
        input_tokens: 10,
        total_tokens: 10,
        ..Default::default()
    };
    session
        .record_terminal_gate_usage(
            octet_ai::EndpointId("prior-provider".into()),
            octet_ai::ModelId("unpriced-tier".into()),
            usage,
            None,
            Some(true),
        )
        .unwrap();
    session
        .record_terminal_gate_usage(
            octet_ai::EndpointId("known-provider".into()),
            octet_ai::ModelId("priced-model".into()),
            usage,
            Some(octet_ai::Cost {
                total: 4_200,
                ..Default::default()
            }),
            Some(true),
        )
        .unwrap();
    assert!(
        !session.has_uncertain_usage(),
        "known tokens, not interrupted usage"
    );
    assert!(session.has_unpriced_usage());
    let mut shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().price_display = PriceDisplay::Priced;
    shell.set_session_telemetry(&session, None);
    assert!(shell.state.borrow().usage_uncertain);
    assert_eq!(shell.state.borrow().session_cost_microdollars, Some(4_200));
    drop(session);
    shell.state.borrow_mut().usage_uncertain = false;
    shell.hydrate(&Session::open(&path).unwrap()).unwrap();
    assert_eq!(shell.state.borrow().price_display, PriceDisplay::Priced);
    assert!(shell.state.borrow().usage_uncertain);
    assert_eq!(shell.state.borrow().session_cost_microdollars, Some(4_200));
    shell.begin_run("known-provider");
    assert!(shell.state.borrow().usage_uncertain);
    let empty = Session::create(directory.path().join("empty.jsonl")).unwrap();
    shell.hydrate(&empty).unwrap();
    assert!(!shell.state.borrow().usage_uncertain);
}

#[test]
fn transcript_link_hit_test_reads_the_last_rendered_row() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 40);
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized("see [docs](https://example.test/docs) now".to_owned()),
        )));
    let (row, column) = {
        let state = shell.state.borrow();
        let transcript = transcript_lines(&state, 100);
        let row = transcript
            .iter()
            .position(|line| line.contains("docs"))
            .expect("the rendered transcript contains the link label");
        let line = &transcript[row];
        assert!(
            line.contains("\x1b]8;;https://example.test/docs"),
            "the markdown renderer emits the OSC 8 link: {line:?}"
        );
        let index = line.find("docs").expect("label");
        (
            u16::try_from(row).unwrap(),
            u16::try_from(visible_width(&line[..index])).unwrap(),
        )
    };
    assert_eq!(
        shell.transcript_link_at_screen_cell(row, column).as_deref(),
        Some("https://example.test/docs"),
        "the link is resolved from the row the frame actually painted"
    );
    assert_eq!(
        shell.transcript_link_at_screen_cell(row, 0),
        None,
        "the marker column is not part of the link"
    );
    assert_eq!(
        shell.transcript_link_at_screen_cell(u16::MAX, column),
        None,
        "pinned chrome rows never report a transcript link"
    );
}

#[test]
fn failed_tool_calls_never_warn_and_live_subagents_are_reported_under_the_outcome() {
    use octet_agent::{EntryId, FinishReason, ToolError};

    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 40);
    let run = shell.begin_run("openai");
    shell.on_prompt_submitted("delegate and run commands");
    // Two failed tool calls inside a run that still completes.
    for (index, name) in ["bash", "edit"].into_iter().enumerate() {
        let id = ToolCallId(format!("call-{index}"));
        shell.on_run_event(
            run,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: name.into(),
                args: serde_json::json!({"command": "false", "path": "src/lib.rs"}),
            },
        );
        shell.on_run_event(
            run,
            &AgentEvent::ToolFinished {
                id,
                result: Err(ToolError::new("command exited 1")),
                duration: Duration::from_millis(10),
            },
        );
    }
    let worker = |state: &str, calls: u64| octet_agent::DelegationTelemetryChild {
        child_id: "agent-1".into(),
        task_name: "inspect-markdown".into(),
        profile: Some("explore".into()),
        model: "test-model".into(),
        state: state.into(),
        phase: state.into(),
        current_tool: (state == "running").then(|| "read".into()),
        tool_use_count: calls,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        estimated_output_tokens: None,
        output_tokens: 10 * calls,
        reasoning_tokens: 0,
        total_tokens: 110,
        cost: None,
        cost_microdollars: Some(1),
        elapsed_ms: 500 * calls,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: test_effective_tool_policy(),
        orchestration_provenance: inherited_delegation_provenance(),
        session: Some("agent-session:opaque".into()),
    };
    let roster =
        |child: octet_agent::DelegationTelemetryChild| octet_agent::DelegationTelemetrySnapshot {
            revision: 1,
            captured_at_ms: 1_700_000_000_000,
            children: vec![child],
            total_cost_microdollars: Some(1),
            failure_reason: None,
            failure_class: None,
        };
    // Workers are alive while the run settles.
    publish_current_turn_roster(&mut shell, roster(worker("running", 2)));
    shell.on_run_event(
        run,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Completed,
        },
    );

    let outcome_index = {
        let state = shell.state.borrow();
        let index = state
            .transcript
            .iter()
            .rposition(|block| matches!(block, TranscriptBlock::Outcome(_)))
            .expect("the completed run appended an outcome block");
        // The warning count still lives in the model; it is never transcript
        // wording.
        let TranscriptBlock::Outcome(block) = &state.transcript[index] else {
            unreachable!()
        };
        assert!(
            matches!(
                block.outcome,
                RunOutcome::CompletedWithWarnings { warnings: 2, .. }
            ),
            "{:?}",
            block.outcome
        );
        index
    };
    let frame = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 100).join("\n"));
    assert!(frame.contains("✓ completed"), "{frame:?}");
    assert!(!frame.to_lowercase().contains("warning"), "{frame:?}");
    assert!(
        !frame.contains("tool call") && !frame.contains("during this run"),
        "{frame:?}"
    );
    assert!(
        frame.contains("Subagents · 1 running · /subagents"),
        "{frame:?}"
    );

    // Worker-only telemetry does not repaint the separate outcome or row.
    let revision = shell.state.borrow().block_revisions[outcome_index];
    let before = shell.state.borrow().rendered_transcript(100).clone();
    publish_current_turn_roster(&mut shell, roster(worker("running", 5)));
    let after = shell.state.borrow().rendered_transcript(100).clone();
    assert_eq!(after.len(), before.len());
    for (old, new) in before.iter().zip(&after) {
        if !old.contains("inspect-markdown") {
            assert_eq!(new, old, "only the live worker metric row may change");
        }
    }
    assert!(strip_terminal_sequences(&after.join("\n")).contains("↓50"));
    assert!(shell_chrome(&shell.state.borrow(), 100, Instant::now())
        .subagents
        .is_empty());
    assert_eq!(
        shell.state.borrow().block_revisions[outcome_index],
        revision,
        "a live worker event must not touch the outcome block"
    );

    // Settlement repaints only the row and leaves the outcome unchanged.
    publish_current_turn_roster(&mut shell, roster(worker("completed", 5)));
    assert!(
        shell.state.borrow().block_revisions[outcome_index] == revision,
        "the independent row settles without repainting the outcome block"
    );
    let frame = strip_terminal_sequences(&render_shell(&shell.state.borrow(), 100).join("\n"));
    assert!(!frame.contains("subagents are running"), "{frame:?}");
    assert!(frame.contains("✓ completed"), "{frame:?}");
    assert!(!frame.to_lowercase().contains("warning"), "{frame:?}");
}

#[test]
fn wide_default_prose_and_prompt_use_the_available_width() {
    let theme = crate::tui::theme::test_theme();
    let source = (0..17).map(|_| "abcdef").collect::<Vec<_>>().join(" ");
    assert!(source.len() > 92 && source.len() < 158);
    let document = parse_markdown(&source);
    let renderer = theme.rich_renderer();
    assert_eq!(
        renderer.render(&document, 158).plain_lines(),
        vec![source.clone()]
    );
    let prompt = render_user_prompt(&source, &None, None, &renderer, &theme, 160);
    assert_eq!(
        prompt.len(),
        1,
        "wide prompt must not retain a 92-cell lane: {prompt:?}"
    );
    assert_eq!(strip_terminal_sequences(&prompt[0]), format!("› {source}"));
    assert!(renderer.render(&document, 80).lines.len() > 1);
}

#[test]
fn colored_prompt_preserves_markdown_runs_inside_its_highlight() {
    let theme = crate::tui::theme::test_theme_for(
        TerminalBackground::Dark,
        crate::tui::terminal::TerminalCapabilities::test(
            true,
            true,
            crate::tui::terminal::ColorDepth::TrueColor,
        ),
    );
    let source = "plain **strong** and `code` [link](https://example.org)";
    let block = TranscriptBlock::User {
        text: source.into(),
        model_lab: Some(ModelLab::OpenAi),
        prompt_color: Some("#123456".into()),
        persisted: true,
    };
    assert_eq!(block_copy_text(&block), source);
    let rows = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        120,
        false,
    );
    let row = rows
        .iter()
        .position(|line| strip_terminal_sequences(line).contains("strong"))
        .expect("rich prompt row");
    let line = strip_terminal_sequences(&rows[row]);
    let terminal = emulate_rows(&rows, 120);
    let plain = terminal
        .screen()
        .cell(row as u16, line.find("plain").unwrap() as u16)
        .unwrap();
    let strong = terminal
        .screen()
        .cell(row as u16, line.find("strong").unwrap() as u16)
        .unwrap();
    let code = terminal
        .screen()
        .cell(row as u16, line.find("code").unwrap() as u16)
        .unwrap();
    let link = terminal
        .screen()
        .cell(row as u16, line.find("link").unwrap() as u16)
        .unwrap();
    assert!(strong.bold(), "strong Markdown lost its weight: {rows:?}");
    assert_ne!(
        code.fgcolor(),
        plain.fgcolor(),
        "inline code lost its color: {rows:?}"
    );
    assert!(
        link.underline(),
        "Markdown link lost its underline: {rows:?}"
    );
    assert_ne!(plain.bgcolor(), vt100::Color::Default);
    for cell in [strong, code, link] {
        assert_eq!(
            cell.bgcolor(),
            plain.bgcolor(),
            "highlight dropped inside an inline span"
        );
    }
}

#[test]
fn live_model_picker_refresh_preserves_filter_and_model_identity() {
    let mut shell = InteractiveShell::test_shell();
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Select model"),
        items: vec!["Alpha".into(), "Beta One".into()],
        descriptions: vec![None, None],
        selected: 0,
        filter: "beta".into(),
        action: PanelAction::SelectGroupedModel {
            models: vec![ModelId("alpha".into()), ModelId("beta-one".into())],
            providers: vec!["one".into(), "one".into()],
            details: Vec::new(),
            scope: None,
        },
    });
    assert_eq!(shell.highlighted_panel_index(), Some(1));
    assert!(shell.refresh_panel_models(
        vec!["Beta Two".into(), "Alpha".into(), "Beta One".into()],
        vec![None, None, None],
        vec![
            ModelId("beta-two".into()),
            ModelId("alpha".into()),
            ModelId("beta-one".into()),
        ],
        vec!["two".into(), "one".into(), "one".into()],
        Vec::new(),
    ));
    assert_eq!(shell.highlighted_panel_index(), Some(2));
    let state = shell.state.borrow();
    let Some(Panel::SelectList {
        filter,
        selected,
        action,
        ..
    }) = state.panel.as_ref()
    else {
        panic!("model picker was replaced");
    };
    assert_eq!(filter, "beta");
    assert_eq!(*selected, 1);
    assert!(
        matches!(action, PanelAction::SelectGroupedModel { models, .. } if models[2].0 == "beta-one")
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn linux_clipboard_writers_follow_the_declared_display() {
    let writers = |names: &[&str]| {
        let names = names.to_vec();
        native_clipboard_writers(move |name| names.contains(&name))
            .into_iter()
            .map(|(program, _)| program)
            .collect::<Vec<_>>()
    };
    assert!(writers(&[]).is_empty(), "no display means no helper");
    assert_eq!(writers(&["WAYLAND_DISPLAY"]), ["wl-copy"]);
    assert_eq!(writers(&["DISPLAY"]), ["xclip", "xsel"]);
    // Hyprland with XWayland declares both; the native Wayland helper wins.
    assert_eq!(
        writers(&["WAYLAND_DISPLAY", "DISPLAY"]),
        ["wl-copy", "xclip", "xsel"]
    );
    assert_eq!(writers(&["TERMUX_VERSION"]), ["termux-clipboard-set"]);
}

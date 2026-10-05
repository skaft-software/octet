//! Session, message, model, and resume pickers, the confirmation panel, the generic select list,
//! and the grouped subagent panel: which rows exist, which row owns the cursor, and which
//! driver request a key press turns into. Separate from the chrome suites because they all
//! drive the same panel state machine through keyboard input.

use super::support::*;

use super::*;

#[test]
fn session_picker_ordering_filters_and_cycles_sorts() {
    let mut named = picker_session("named", "Zebra", 2, 30);
    named.name = Some("Release notes".into());
    named.tags = vec!["rust".into()];
    let rows = vec![
        picker_session("recent", "Beta", 1, 40),
        named,
        picker_session("long", "Alpha", 9, 20),
    ];
    let mut picker = PickerState::new(rows, None);

    assert_eq!(session_picker_ordering(&picker), vec![0, 1, 2]);
    picker.sort = PickerSort::Name;
    assert_eq!(session_picker_ordering(&picker), vec![2, 0, 1]);
    picker.sort = PickerSort::Messages;
    assert_eq!(session_picker_ordering(&picker), vec![2, 1, 0]);

    picker.named_only = true;
    assert_eq!(session_picker_ordering(&picker), vec![1]);
    picker.named_only = false;
    picker.filter = "rse".into();
    assert_eq!(session_picker_ordering(&picker), vec![1]);
    picker.filter = "re:beta".into();
    assert_eq!(session_picker_ordering(&picker), vec![0]);
}

#[test]
fn session_picker_panel_handles_scope_filter_and_selection_outbox() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 24);
    let mut first = picker_session("one", "First", 1, 1);
    first.name = Some("First name".into());
    let rows = vec![first, picker_session("two", "Second", 2, 2)];
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(rows.clone(), Some(rows[0].path.clone()))),
    });

    shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
    shell.panel_input(&panel_key_with_modifiers(
        crossterm::event::KeyCode::Char('n'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    let state = shell.state.borrow();
    let Some(Panel::SessionPicker { picker }) = state.panel.as_ref() else {
        panic!("session picker should be open");
    };
    assert_eq!(picker.selected, 0, "named-filter changes reset selection");
    drop(state);

    // Restore the complete list; the current row is protected from deletion.
    shell.panel_input(&panel_key_with_modifiers(
        crossterm::event::KeyCode::Char('n'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Delete));
    let state = shell.state.borrow();
    let Some(Panel::SessionPicker { picker }) = state.panel.as_ref() else {
        panic!("session picker should be open");
    };
    assert!(!picker.confirming_delete);
    let OrdinarySurfaceLifecycle::RecoverableError(status) = &picker.surface.lifecycle else {
        panic!("current-session delete should set a recoverable-error lifecycle");
    };
    assert_eq!(status.text, "cannot delete the currently active session");
    drop(state);

    // Clear the named/filter state and select the second row.
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Char('x')));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
    let (result, action) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("session selection should close the panel");
    assert_eq!(result, PanelResult::Select("two".into()));
    assert!(matches!(action, PanelAction::SessionPicker));
    assert_eq!(
        shell.take_picker_selection(),
        Some(("two".into(), PathBuf::from("/tmp/two.jsonl")))
    );
    assert!(!shell.has_panel());
}

#[test]
fn session_picker_rename_and_delete_emit_driver_requests() {
    let mut shell = InteractiveShell::test_shell();
    let row = picker_session("one", "First", 1, 1);
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(vec![row], None)),
    });

    shell.panel_input(&panel_key_with_modifiers(
        crossterm::event::KeyCode::Char('r'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Char('X')));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Enter));
    assert!(matches!(
        shell.drain_panel_requests().as_slice(),
        [PanelRequest::RenameSession { id, name, .. }] if id == "one" && name == "X"
    ));

    shell.panel_input(&panel_key(crossterm::event::KeyCode::Delete));
    assert!(shell.has_panel());
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Enter));
    assert!(matches!(
        shell.drain_panel_requests().as_slice(),
        [PanelRequest::TrashSession { id, .. }] if id == "one"
    ));
}

#[test]
fn session_picker_trashes_by_portable_chord_with_a_named_confirmation() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 24);
    let mut named = picker_session("one", "First", 1, 9);
    named.name = Some("Release notes".into());
    let rows = vec![named, picker_session("two", "Second", 2, 2)];
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(rows.clone(), None)),
    });
    let trash = panel_key_with_modifiers(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    let prompt = |shell: &InteractiveShell| {
        strip_terminal_sequences(&render_panel(&shell.state.borrow(), 100).join("\n"))
            .lines()
            .find(|line| line.contains("to trash?"))
            .map(str::to_owned)
    };

    // Ctrl-X works without a forward Delete key; the prompt names the
    // session, and Esc leaves it untouched.
    shell.panel_input(&trash);
    let asked = prompt(&shell).expect("the confirmation should be rendered");
    assert!(asked.contains("Release notes"), "{asked}");
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Esc));
    assert!(shell.drain_panel_requests().is_empty());
    assert!(prompt(&shell).is_none());
    assert!(shell.has_panel());

    shell.panel_input(&trash);
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Enter));
    assert!(matches!(
        shell.drain_panel_requests().as_slice(),
        [PanelRequest::TrashSession { id, .. }] if id == "one"
    ));

    // The driver refreshes the rows; focus lands on a remaining session and
    // Enter can only resume that one.
    shell.refresh_panel_sessions(vec![rows[1].clone()], None);
    let (result, _) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("a remaining session should be selectable");
    assert_eq!(result, PanelResult::Select("two".into()));
}

#[test]
fn session_picker_trashing_the_last_session_leaves_an_inert_empty_list() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 24);
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(
            vec![picker_session("only", "Only", 1, 1)],
            None,
        )),
    });
    let trash = panel_key_with_modifiers(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    shell.panel_input(&trash);
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Enter));
    assert!(matches!(
        shell.drain_panel_requests().as_slice(),
        [PanelRequest::TrashSession { id, .. }] if id == "only"
    ));

    shell.refresh_panel_sessions(Vec::new(), None);
    assert!(shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .is_none());
    shell.panel_input(&trash);
    let state = shell.state.borrow();
    let Some(Panel::SessionPicker { picker }) = state.panel.as_ref() else {
        panic!("session picker should stay open");
    };
    assert!(!picker.confirming_delete);
    drop(state);
    assert!(shell.drain_panel_requests().is_empty());
}

#[test]
fn message_picker_returns_selected_text_through_outbox() {
    let mut shell = InteractiveShell::test_shell();
    shell.open_panel(Panel::MessagePicker {
        picker: MessagePicker::new(vec![
            ForkMessage {
                entry_id: "entry-a".into(),
                text: "first prompt".into(),
                whole_conversation: false,
            },
            ForkMessage {
                entry_id: "entry-head".into(),
                text: String::new(),
                whole_conversation: true,
            },
        ]),
    });
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Up));
    let (result, action) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("message selection should close the panel");
    assert_eq!(result, PanelResult::Select("entry-a".into()));
    assert!(matches!(action, PanelAction::MessagePicker));
    assert_eq!(
        shell.take_message_picker_selection(),
        Some(("entry-a".into(), "first prompt".into()))
    );
}

#[test]
fn session_picker_render_shows_scope_markers_and_fork_metadata() {
    let mut shell = InteractiveShell::test_shell();
    let mut fork = picker_session("fork", "Forked", 3, 1);
    fork.pinned = true;
    fork.forked_from_session_id = Some("source".into());
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(vec![fork], None)),
    });
    let raw = render_panel(&shell.state.borrow(), 100);
    let plain = raw
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    let rendered = plain.join("\n");
    assert!(rendered.contains("Resume Session (Current Folder)"));
    assert!(rendered.contains("Forked (fork)"));
    assert!(!rendered.contains("(current)"));
    assert!(rendered.contains("^s sort"));
    assert_eq!(raw.join("\n").matches(CURSOR_MARKER).count(), 1);
    assert!(plain[1].starts_with("Resume Session"), "{plain:?}");
    assert!(plain[2].starts_with("Select a saved session"), "{plain:?}");
    assert!(plain[3].starts_with("Filter"), "{plain:?}");
    let selected = plain
        .iter()
        .find(|line| line.contains("Forked (fork)"))
        .expect("selected session title");
    assert!(
        selected.starts_with("› ") || selected.starts_with("> "),
        "{plain:?}"
    );
}

#[test]
fn model_and_resume_pickers_use_the_active_model_accent() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    let (model_accent, ui_accent) = {
        let state = shell.state.borrow();
        let sequence = |role| {
            let (red, green, blue) = state
                .theme
                .role_rgb(role)
                .unwrap_or_else(|| panic!("missing {role} colour"));
            format!("\x1b[38;2;{red};{green};{blue}m")
        };
        (sequence("model_accent"), sequence("accent"))
    };
    assert_ne!(model_accent, ui_accent);

    open_select_panel(&mut shell, &["Claude Sonnet 4", "GPT-5"]);
    let model_rows = render_panel(&shell.state.borrow(), 80);
    let selected_model = model_rows
        .iter()
        .find(|line| strip_terminal_sequences(line).contains("Claude Sonnet 4"))
        .expect("selected model row");
    assert!(selected_model.contains(&model_accent), "{model_rows:?}");
    assert!(!selected_model.contains(&ui_accent), "{model_rows:?}");

    shell.close_panel();
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(
            vec![picker_session("one", "First session", 1, 1)],
            None,
        )),
    });
    let resume_rows = render_panel(&shell.state.borrow(), 100);
    let selected_session = resume_rows
        .iter()
        .find(|line| strip_terminal_sequences(line).contains("First session"))
        .expect("selected resume row");
    assert!(selected_session.contains(&model_accent), "{resume_rows:?}");
    assert!(!selected_session.contains(&ui_accent), "{resume_rows:?}");
    let active_scope = resume_rows
        .iter()
        .find(|line| strip_terminal_sequences(line).contains("Current Folder"))
        .expect("active resume scope");
    assert!(active_scope.contains(&model_accent), "{resume_rows:?}");
    assert!(!active_scope.contains(&ui_accent), "{resume_rows:?}");
}

#[test]
fn wide_session_picker_gives_titles_and_metadata_separate_rows() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 24);
    let title =
        "please perform a thorough audit of the resume picker layout without truncating the title";
    let mut unreadable = picker_session("2026-08-27-session-00ff", "(unreadable session)", 0, 2);
    unreadable.modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(9);
    let mut readable = picker_session("readable", title, 12, 1);
    readable.modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10);
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(vec![readable, unreadable], None)),
    });

    let lines = render_panel(&shell.state.borrow(), 100)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    let title_row = lines
        .iter()
        .position(|line| line.contains(title))
        .expect("the wide title should not be truncated");
    assert!(lines[title_row + 1].contains("12 msgs"));
    assert!(lines
        .iter()
        .any(|line| { line.contains("(unreadable session ·") && line.contains("00ff)") }));
    assert!(lines.iter().all(|line| visible_width(line) <= 100));
}

#[test]
fn session_picker_hides_advanced_filter_hints_until_used() {
    let mut shell = InteractiveShell::test_shell();
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(PickerState::new(
            vec![picker_session("one", "First session", 1, 1)],
            None,
        )),
    });
    let plain = strip_terminal_sequences(&render_panel(&shell.state.borrow(), 100).join("\n"));
    assert!(plain.contains("^f transcripts"), "{plain:?}");
    assert!(plain.contains("^s sort"), "{plain:?}");
    assert!(!plain.contains("re:<pattern>"), "{plain:?}");

    let mut advanced = PickerState::new(vec![picker_session("one", "First session", 1, 1)], None);
    advanced.filter = "re:foo".into();
    shell.close_panel();
    shell.open_panel(Panel::SessionPicker {
        picker: Box::new(advanced),
    });
    let plain = strip_terminal_sequences(&render_panel(&shell.state.borrow(), 100).join("\n"));
    assert!(plain.contains("re:<pattern>"), "{plain:?}");
}

#[test]
fn confirmation_panel_shows_shared_detail_and_unfiltered_actions() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 24);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Approve one exact `bash` tool effect?"),
        items: vec!["Deny".into(), "Approve".into()],
        descriptions: vec![
            Some("effect: host_process sha256: 85bc9fe8cfaf7c550880d65882f7c4142c8374875c976c58d1dd724a7f16e609".into()),
            Some("effect: host_process sha256: 85bc9fe8cfaf7c550880d65882f7c4142c8374875c976c58d1dd724a7f16e609".into()),
        ],
        selected: 0,
        filter: String::new(),
        action: PanelAction::Confirmation,
    });

    let lines = render_panel(&shell.state.borrow(), 100)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    let rendered = lines.join("\n");
    assert_eq!(lines.len(), 5);
    assert!(rendered.contains("Approve one exact `bash` tool effect?"));
    assert!(rendered.contains("Deny"));
    assert!(rendered.contains("Approve"));
    assert!(rendered.contains("Detail"));
    assert_eq!(rendered.matches("85bc9fe8").count(), 1);
    assert!(!rendered.contains("Filter"));
    assert!(!rendered.contains("1/2"));

    shell.panel_input(&panel_key(crossterm::event::KeyCode::Char('x')));
    assert!(panel_state(&shell).2.is_empty());
}

#[test]
fn confirmation_requires_its_selected_action_to_be_visible() {
    for (selected, label) in [(0, "Deny"), (1, "Approve")] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 5);
        shell.open_panel(Panel::SelectList {
            surface: OrdinarySurfaceMetadata::new("Approve?"),
            items: vec!["Deny".into(), "Approve".into()],
            descriptions: vec![Some("writes src/lib.rs".into()); 2],
            selected,
            filter: String::new(),
            action: PanelAction::Confirmation,
        });

        let hidden = shell_chrome(&shell.state.borrow(), 80, Instant::now()).panel;
        assert_eq!(hidden.len(), 1, "{hidden:?}");
        assert_eq!(strip_terminal_sequences(&hidden[0]).trim(), "Approve?");
        assert!(
            shell
                .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
                .is_none(),
            "Enter must not choose an action that is not rendered"
        );
        assert!(shell.has_panel());

        shell.set_size(80, 6);
        let visible = shell_chrome(&shell.state.borrow(), 80, Instant::now()).panel;
        assert_eq!(visible.len(), 2, "{visible:?}");
        assert!(visible.iter().any(|line| line.contains(label)));
        let (result, action) = shell
            .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
            .expect("visible selected action can be chosen");
        assert_eq!(result, PanelResult::Confirm(selected));
        assert!(matches!(action, PanelAction::Confirmation));
    }
}

#[test]
fn confirmation_allows_a_visible_action_when_the_title_is_clipped() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(46, 8);
    let title = "Approve one exact workspace mutation with a deliberately long identity?";
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new(title),
        items: vec!["Deny".into(), "Approve".into()],
        descriptions: vec![None, None],
        selected: 1,
        filter: String::new(),
        action: PanelAction::Confirmation,
    });

    let visible = shell_chrome(&shell.state.borrow(), 46, Instant::now()).panel;
    assert!(
        visible.iter().any(|line| line.contains("Approve")),
        "{visible:?}"
    );
    assert!(
        visible
            .iter()
            .all(|line| !strip_terminal_sequences(line).contains(title)),
        "the test requires a clipped title: {visible:?}"
    );
    let (result, action) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("a visible action remains actionable when the title is clipped");
    assert_eq!(result, PanelResult::Confirm(1));
    assert!(matches!(action, PanelAction::Confirmation));
}

#[test]
fn select_list_filter_narrows_items_and_confirm_returns_original_index() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    open_select_panel(&mut shell, &["alpha", "beta", "gamma"]);

    for c in "amm".chars() {
        assert!(
            shell
                .panel_input(&panel_key(crossterm::event::KeyCode::Char(c)))
                .is_none(),
            "typing must keep the panel open"
        );
    }

    let rendered = render_shell(&shell.state.borrow(), 80).join("\n");
    assert!(rendered.contains("gamma"), "matching item must render");
    assert!(
        !rendered.contains("alpha"),
        "filtered-out item must not render"
    );
    assert!(
        !rendered.contains("beta"),
        "filtered-out item must not render"
    );

    let (result, _) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("enter should confirm the sole match");
    // "gamma" is index 2 in the original list.
    assert_eq!(result, PanelResult::Confirm(2));
    assert!(!shell.has_panel());
}

#[test]
fn selected_worker_remains_visible_when_it_settles_without_revealing_its_siblings() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(120, 24);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Subagents"),
        items: vec!["alpha".into(), "beta".into()],
        descriptions: vec![None, None],
        selected: 1,
        filter: String::new(),
        action: PanelAction::SelectSubagent(SubagentPanel {
            node_ids: vec!["node-a".into(), "node-b".into()],
            groups: vec![SubagentGroup {
                label: "Running".into(),
                indices: vec![0, 1],
                collapsible: false,
            }],
            collapsed: true,
            revealed_node: None,
            state_filter: None,
        }),
    });
    for _ in 0..2 {
        shell.refresh_subagent_panel(
            "Subagents".into(),
            vec!["beta".into(), "gamma".into(), "hidden-sibling".into()],
            vec![None, None, None],
            SubagentPanel {
                node_ids: vec!["node-b".into(), "node-c".into(), "node-d".into()],
                groups: vec![
                    SubagentGroup {
                        label: "Running".into(),
                        indices: vec![1],
                        collapsible: false,
                    },
                    SubagentGroup {
                        label: "Done".into(),
                        indices: vec![0, 2],
                        collapsible: true,
                    },
                ],
                collapsed: true,
                revealed_node: None,
                state_filter: None,
            },
        );
        // Exercise both the natural-height test wrapper and the bounded
        // renderer used by production chrome. Both groups fit in this budget.
        let rendered = {
            let state = shell.state.borrow();
            [
                render_panel(&state, 120),
                super::panel_render::render_panel_with_limit(&state, 120, 20),
            ]
        };
        for rows in rendered {
            assert!(rows.len() <= 20);
            let painted = strip_terminal_sequences(&rows.join("\n"));
            assert!(
                painted.contains("beta") && painted.contains("gamma"),
                "{painted}"
            );
            assert!(!painted.contains("hidden-sibling"), "{painted}");
        }
    }
    let (result, action) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .unwrap();
    assert_eq!(result, PanelResult::Confirm(0));
    assert!(matches!(action, PanelAction::SelectSubagent(panel) if panel.node_ids[0] == "node-b"));
}

#[test]
fn live_subagent_refresh_preserves_selection_by_stable_node_id() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Subagents"),
        items: vec!["alpha".into(), "beta".into()],
        descriptions: vec![Some("running".into()), Some("done".into())],
        selected: 1,
        filter: String::new(),
        action: PanelAction::SelectSubagent(super::SubagentPanel {
            node_ids: vec!["node-a".into(), "node-b".into()],
            groups: vec![
                super::SubagentGroup {
                    label: "Running".into(),
                    indices: vec![0],
                    collapsible: false,
                },
                super::SubagentGroup {
                    label: "Done".into(),
                    indices: vec![1],
                    collapsible: true,
                },
            ],
            collapsed: true,
            revealed_node: None,
            state_filter: None,
        }),
    });

    shell.refresh_subagent_panel(
        "Subagents · refreshed".into(),
        vec!["beta".into(), "gamma".into()],
        vec![Some("done".into()), Some("running".into())],
        super::SubagentPanel {
            node_ids: vec!["node-b".into(), "node-c".into()],
            groups: vec![
                super::SubagentGroup {
                    label: "Running".into(),
                    indices: vec![0],
                    collapsible: false,
                },
                super::SubagentGroup {
                    label: "Done".into(),
                    indices: vec![1],
                    collapsible: true,
                },
            ],
            collapsed: true,
            revealed_node: None,
            state_filter: None,
        },
    );

    let (result, action) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("enter should confirm the stable refreshed selection");
    assert_eq!(result, PanelResult::Confirm(0));
    assert!(matches!(
        action,
        PanelAction::SelectSubagent(panel) if panel.node_ids == ["node-b", "node-c"]
    ));
}

#[test]
fn subagent_panel_groups_states_and_collapses_finished_workers_by_default() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(120, 40);
    // 8 live workers and 6 finished ones, mirroring the reported 32-row panel.
    open_grouped_subagent_panel(&mut shell, 8, 2);

    let rows = render_panel(&shell.state.borrow(), 120);
    let plain = strip_terminal_sequences(&rows.join("\n"));
    // Live work is expanded, with its own counted heading.
    assert!(plain.contains("Running · 8"), "{plain}");
    for index in 0..8 {
        assert!(plain.contains(&format!("live-{index}")), "{plain}");
    }
    // Finished work is collapsed behind one bounded, counted summary line.
    for hidden in [
        "done-0",
        "failed-0",
        "stopped-0",
        "done-1",
        "failed-1",
        "stopped-1",
    ] {
        assert!(!plain.contains(hidden), "collapsed row rendered: {plain}");
    }
    assert!(plain.contains("2 Done"), "{plain}");
    assert!(plain.contains("2 Failed"), "{plain}");
    assert!(plain.contains("2 Stopped"), "{plain}");
    assert!(plain.contains("ctrl+t shows all"), "{plain}");
    // Bounded: the panel never renders past its row allowance.
    assert!(rows.len() <= 36, "{} panel rows", rows.len());

    // The toggle expands every group with its own counted heading.
    assert!(shell
        .panel_input(&panel_key_with_modifiers(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::CONTROL,
        ))
        .is_none());
    let rows = render_panel(&shell.state.borrow(), 120);
    let plain = strip_terminal_sequences(&rows.join("\n"));
    for visible in ["done-0", "failed-1", "stopped-1"] {
        assert!(plain.contains(visible), "{plain}");
    }
    assert!(plain.contains("Done · 2"), "{plain}");
    assert!(plain.contains("Failed · 2"), "{plain}");
    assert!(plain.contains("Stopped · 2"), "{plain}");
    assert!(!plain.contains("ctrl+t shows all"), "{plain}");

    // Collapsing again still lets a typed filter reach a hidden worker.
    assert!(shell
        .panel_input(&panel_key_with_modifiers(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::CONTROL,
        ))
        .is_none());
    for character in "failed-1".chars() {
        shell.panel_input(&panel_key(crossterm::event::KeyCode::Char(character)));
    }
    let plain = strip_terminal_sequences(&render_panel(&shell.state.borrow(), 120).join("\n"));
    assert!(plain.contains("failed-1"), "{plain}");
    assert!(
        !plain.contains("live-0"),
        "the filter still narrows live rows: {plain}"
    );
}

#[test]
fn subagent_panel_refresh_keeps_collapsed_groups_and_a_visible_selection() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(110, 40);
    open_grouped_subagent_panel(&mut shell, 2, 1);
    // Select the second live worker, then refresh with a new revision whose live
    // worker order changed and whose finished worker count grew.
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
    let selected_id = {
        let state = shell.state.borrow();
        let Some(Panel::SelectList {
            action, selected, ..
        }) = state.panel.as_ref()
        else {
            panic!("subagent panel is open");
        };
        let panel = action.subagent_panel().expect("subagent action");
        panel.node_ids[(*selected).min(panel.node_ids.len() - 1)].clone()
    };

    let items = vec![
        "live-0".to_owned(),
        "live-1".to_owned(),
        "done-0".to_owned(),
        "done-1".to_owned(),
    ];
    let descriptions = items.iter().map(|_| Some("48s".to_owned())).collect();
    shell.refresh_subagent_panel(
        "Subagents · refreshed".into(),
        items,
        descriptions,
        super::SubagentPanel {
            node_ids: vec![
                "node-live-0".into(),
                "node-live-1".into(),
                "node-done-0".into(),
                "node-done-1".into(),
            ],
            groups: vec![
                super::SubagentGroup {
                    label: "Running".into(),
                    indices: vec![0, 1],
                    collapsible: false,
                },
                super::SubagentGroup {
                    label: "Done".into(),
                    indices: vec![2, 3],
                    collapsible: true,
                },
            ],
            collapsed: false,
            revealed_node: None,
            state_filter: None,
        },
    );

    let state = shell.state.borrow();
    let Some(Panel::SelectList {
        action,
        selected,
        filter,
        items,
        ..
    }) = state.panel.as_ref()
    else {
        panic!("subagent panel is still open");
    };
    let panel = action.subagent_panel().expect("subagent action");
    // The refresh never re-opens a group the reader collapsed.
    assert!(panel.collapsed);
    assert_eq!(panel.group_of(2), Some(1));
    // Selection stays on the same stable node and on a row that is visible.
    assert_eq!(panel.node_ids[*selected], selected_id);
    assert!(!panel.hides(*selected));
    assert!(filter.is_empty());
    let plain = strip_terminal_sequences(&render_panel(&state, 110).join("\n"));
    assert!(plain.contains(&items[*selected]), "{plain}");
    assert!(!plain.contains("done-1"), "{plain}");
}

#[test]
fn select_list_filter_is_case_insensitive_and_matches_descriptions() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Select model"),
        items: vec!["gpt-4o".into(), "claude-sonnet".into()],
        descriptions: vec![
            Some("openai · 128k context".into()),
            Some("anthropic · 200k context".into()),
        ],
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectModel(vec![]),
    });

    // Multi-term uppercase query must match across label + description.
    for c in "CLAUDE ANTHROPIC".chars() {
        shell.panel_input(&panel_key(crossterm::event::KeyCode::Char(c)));
    }
    let rendered = render_shell(&shell.state.borrow(), 80).join("\n");
    assert!(rendered.contains("claude-sonnet"));
    assert!(!rendered.contains("gpt-4o"));

    let (result, _) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("enter should confirm the description match");
    assert_eq!(result, PanelResult::Confirm(1));
}

#[test]
fn select_list_filter_resets_cursor_and_bounds_navigation() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    open_select_panel(&mut shell, &["apple", "banana", "cherry"]);

    // Move to the last row, then filter: the cursor must restart at the
    // first match.
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Char('a')));
    let (_, selected, filter) = panel_state(&shell);
    assert_eq!(filter, "a");
    assert_eq!(
        selected, 0,
        "typing must reset the cursor to the first match"
    );

    // 'a' matches "apple" and "banana" only; one Down moves to the second
    // match, and a further Down is out of bounds.
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Down));
    let (_, selected, _) = panel_state(&shell);
    assert_eq!(selected, 1, "navigation must stop at the last match");

    let (result, _) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
        .expect("enter should confirm the second match");
    assert_eq!(result, PanelResult::Confirm(1));
}

#[test]
fn select_list_accepts_held_key_repeats_but_ignores_release() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    open_select_panel(&mut shell, &["alpha", "beta", "gamma"]);

    shell.panel_input(&panel_key_kind(
        crossterm::event::KeyCode::Down,
        crossterm::event::KeyEventKind::Repeat,
    ));
    assert_eq!(panel_state(&shell).1, 1);
    shell.panel_input(&panel_key_kind(
        crossterm::event::KeyCode::Down,
        crossterm::event::KeyEventKind::Release,
    ));
    assert_eq!(panel_state(&shell).1, 1);

    assert!(
        shell
            .panel_input(&panel_key_kind(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyEventKind::Repeat,
            ))
            .is_none(),
        "a held Enter key must not confirm a panel twice"
    );
    assert!(shell.has_panel());
}

#[test]
fn select_list_filter_without_matches_keeps_panel_open_on_enter() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    open_select_panel(&mut shell, &["apple", "banana", "cherry"]);

    for c in "zzz".chars() {
        shell.panel_input(&panel_key(crossterm::event::KeyCode::Char(c)));
    }
    let rendered = render_shell(&shell.state.borrow(), 80).join("\n");
    assert!(
        rendered.contains("no matches") && rendered.contains("zzz"),
        "{rendered}"
    );

    // Enter is a no-op while nothing matches; Esc still cancels.
    assert!(
        shell
            .panel_input(&panel_key(crossterm::event::KeyCode::Enter))
            .is_none(),
        "enter must not confirm when no item matches"
    );
    assert!(shell.has_panel());

    // Deleting the filter restores the full list.
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Backspace));
    let rendered = render_shell(&shell.state.borrow(), 80).join("\n");
    assert!(rendered.contains("apple"));
    assert!(rendered.contains("cherry"));

    let (result, _) = shell
        .panel_input(&panel_key(crossterm::event::KeyCode::Esc))
        .expect("esc should cancel the panel");
    assert_eq!(result, PanelResult::Cancel);
}

#[test]
fn select_list_has_a_stable_filter_row_and_owns_the_only_cursor() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    open_select_panel(&mut shell, &["alpha", "beta", "gamma"]);

    let empty_panel_rows = render_panel(&shell.state.borrow(), 80).len();
    let empty = render_shell(&shell.state.borrow(), 80).join("\n");
    let empty_plain = strip_terminal_sequences(&empty);
    assert!(empty_plain.contains("Filter"));
    assert!(empty_plain.contains("type to filter"));
    assert!(empty_plain.contains("1/3"));
    assert_eq!(empty.matches(CURSOR_MARKER).count(), 1);

    shell.panel_input(&panel_key(crossterm::event::KeyCode::Char('a')));
    let filtered_panel_rows = render_panel(&shell.state.borrow(), 80).len();
    let filtered = render_shell(&shell.state.borrow(), 80).join("\n");
    let filtered_plain = strip_terminal_sequences(&filtered);
    assert_eq!(filtered_panel_rows, empty_panel_rows);
    assert!(filtered_plain.contains("Filter  a"));
    assert!(filtered_plain.contains("1/3"));
    assert_eq!(filtered.matches(CURSOR_MARKER).count(), 1);

    shell.close_panel();
    let composer = render_shell(&shell.state.borrow(), 80).join("\n");
    assert_eq!(composer.matches(CURSOR_MARKER).count(), 1);
}

#[test]
fn composer_keeps_its_cursor_marker_at_extreme_narrow_widths() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("anthropic", "claude-sonnet-4", "high");
    let model_rgb = shell
        .state
        .borrow()
        .theme
        .model_rgb(Some(ModelLab::Anthropic))
        .expect("Anthropic model accent");
    let encoded_model_rgb = format!("38;2;{};{};{}", model_rgb.0, model_rgb.1, model_rgb.2);
    for width in [1, 2] {
        let rendered = crate::tui::composer_surface::render_composer_surface(
            &shell.state.borrow(),
            width,
            Instant::now(),
        )
        .join("\n");
        assert_eq!(
            rendered.matches(CURSOR_MARKER).count(),
            1,
            "width {width}: {rendered:?}"
        );
        assert!(
            rendered.contains(&encoded_model_rgb),
            "width {width} lost next-model provenance: {rendered:?}"
        );
    }

    open_select_panel(&mut shell, &["alpha"]);
    let rendered = crate::tui::composer_surface::render_composer_surface(
        &shell.state.borrow(),
        2,
        Instant::now(),
    )
    .join("\n");
    assert_eq!(rendered.matches(CURSOR_MARKER).count(), 0);
}

#[test]
fn select_list_long_filter_keeps_its_tail_and_cursor_in_narrow_panes() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(24, 12);
    open_select_panel(&mut shell, &["alpha", "beta", "gamma"]);
    for character in "abcdefghijklmnopqrstuvwxyz".chars() {
        shell.panel_input(&panel_key(crossterm::event::KeyCode::Char(character)));
    }

    let rendered = render_shell(&shell.state.borrow(), 24).join("\n");
    assert_eq!(rendered.matches(CURSOR_MARKER).count(), 1);
    let cursor_line = rendered
        .lines()
        .find(|line| line.contains(CURSOR_MARKER))
        .expect("the active filter must own the cursor");
    let plain = strip_terminal_sequences(cursor_line).replace(CURSOR_MARKER, "");
    assert!(plain.contains("wxyz"), "{plain:?}");
    assert!(!plain.contains("abcdef"), "{plain:?}");
    assert!(visible_width(&plain) <= 24, "{plain:?}");
}

#[test]
fn select_list_keeps_a_focused_filter_row_in_a_tiny_busy_terminal() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(20, 5);
    shell.error("a wrapped background error that would otherwise consume the picker row".into());
    open_select_panel(&mut shell, &["alpha", "beta", "gamma"]);

    let rendered = render_shell(&shell.state.borrow(), 20);
    assert_eq!(rendered.len(), 5, "{rendered:?}");
    assert_eq!(rendered.join("\n").matches(CURSOR_MARKER).count(), 1);
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Filter") && line.contains(CURSOR_MARKER)),
        "{rendered:?}"
    );
}

#[test]
fn select_list_separates_model_metadata_and_drops_it_before_narrow_labels() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(100, 24);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Select model"),
        items: vec![
            "GPT-5.6".into(),
            "Claude Opus 4.8".into(),
            "Qwen3.6 35B A3B".into(),
        ],
        descriptions: vec![
            Some("openai · 400k context".into()),
            Some("anthropic · 1M context".into()),
            Some("openrouter · 256k context".into()),
        ],
        selected: 1,
        filter: String::new(),
        action: PanelAction::SelectModel(vec![]),
    });

    let wide = render_panel(&shell.state.borrow(), 100)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    assert!(wide[1].starts_with("Select model"), "{wide:?}");
    assert!(wide[2].starts_with("Filter"), "{wide:?}");
    let description_columns = ["openai", "anthropic", "openrouter"]
        .iter()
        .map(|provider| {
            wide.iter()
                .find_map(|line| line.find(provider).map(|byte| visible_width(&line[..byte])))
                .expect("provider metadata should be visible")
        })
        .collect::<Vec<_>>();
    assert!(
        description_columns
            .windows(2)
            .all(|columns| columns[0] == columns[1]),
        "{wide:?}"
    );
    let gpt_row = wide
        .iter()
        .position(|line| line.contains("GPT-5.6"))
        .expect("model label should render");
    assert!(wide[gpt_row + 1].contains("openai"), "{wide:?}");
    assert!(!wide[gpt_row].contains("openai"));
    let selected = wide
        .iter()
        .find(|line| line.contains("Claude Opus"))
        .expect("selected model should render");
    assert!(selected.trim_start().starts_with('›') || selected.trim_start().starts_with('>'));
    assert!(
        selected.starts_with("› ") || selected.starts_with("> "),
        "{wide:?}"
    );
    assert!(wide[gpt_row + 1].starts_with("  "), "{wide:?}");

    let narrow = render_panel(&shell.state.borrow(), 30)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    assert!(narrow.iter().any(|line| line.contains("Claude Opus")));
    assert!(!narrow.iter().any(|line| line.contains("openrouter")));
    assert!(narrow.iter().all(|line| visible_width(line) <= 30));
}

#[test]
fn select_list_home_end_and_page_navigation_stay_bounded() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(52, 12);
    let items = (0..60)
        .map(|index| format!("Model {index:02}"))
        .collect::<Vec<_>>();
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Select model"),
        descriptions: vec![Some("provider · context".into()); items.len()],
        items,
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectModel(vec![]),
    });

    shell.panel_input(&panel_key(crossterm::event::KeyCode::End));
    assert_eq!(panel_state(&shell).1, 59);
    let at_end = render_panel(&shell.state.borrow(), 52)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    assert!(at_end.iter().any(|line| line.contains("60/60")));
    assert!(at_end.iter().any(|line| line.contains("Model 59")));
    assert!(at_end.iter().all(|line| visible_width(line) <= 52));

    shell.panel_input(&panel_key(crossterm::event::KeyCode::PageUp));
    assert_eq!(panel_state(&shell).1, 55);
    shell.panel_input(&panel_key(crossterm::event::KeyCode::PageDown));
    assert_eq!(panel_state(&shell).1, 59);
    shell.panel_input(&panel_key(crossterm::event::KeyCode::Home));
    assert_eq!(panel_state(&shell).1, 0);
}

#[test]
fn secret_tool_prompt_temporarily_owns_composer_without_touching_the_editor() {
    let mut shell = InteractiveShell::test_shell();
    for character in "ordinary draft".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    shell.set_tool_input_prompt(Some("Password:".into()));
    let secret_surface = crate::tui::composer_surface::render_composer_surface(
        &shell.state.borrow(),
        80,
        Instant::now(),
    )
    .iter()
    .map(|line| strip_terminal_sequences(line))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(secret_surface.contains("Password:"), "{secret_surface}");
    assert!(
        !secret_surface.contains("ordinary draft"),
        "{secret_surface}"
    );
    assert_eq!(shell.pending(), "ordinary draft");

    shell.set_tool_input_prompt(None);
    let restored = crate::tui::composer_surface::render_composer_surface(
        &shell.state.borrow(),
        80,
        Instant::now(),
    )
    .iter()
    .map(|line| strip_terminal_sequences(line))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(restored.contains("ordinary draft"), "{restored}");
}

//! RAIL ownership/privacy regressions. Captures below are explicitly test fixtures.
use super::*;
use crate::tui::view::{tern_picker, tern_prompt};
use crossterm::event::{Event as InputEvent, KeyCode};

fn accepting(shell: &InteractiveShell) -> crate::tui::view::tern_input::Handler {
    shell.state.native().lock().unwrap().accepting_input = true;
    shell.tern_input_handler()
}
fn action(id: String, act: &str) -> Incoming {
    Incoming::Event(Event::Action {
        sf: SURFACE.into(),
        id,
        act: act.into(),
        value: None,
        mods: None,
    })
}
fn edit(id: String, text: &str) -> Incoming {
    Incoming::Event(Event::Edit {
        sf: SURFACE.into(),
        id,
        from: 0,
        to: 0,
        text: text.into(),
        cursor: text.encode_utf16().count(),
        len: 0,
    })
}
fn choices(shell: &mut InteractiveShell) {
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::with_purpose(
            "Fixture consent",
            "No real grant is issued",
        ),
        items: vec!["Deny".into(), "Approve fixture only".into()],
        descriptions: vec![Some("Inert test decision".into()); 2],
        selected: 1,
        filter: String::new(),
        action: PanelAction::Confirmation,
    });
}

#[test]
fn markdown_spacing_does_not_rewrite_other_fences_or_live_code() {
    for source in [
        "text\n\n~~~rust\na\n\n\nb\n~~~\nafter",
        "````rust\na\n```\n\n\nb\n````",
        "```rust\na\n\n\n", // unfinished streaming code retains its tail
        "    a\n\n\n    b",
    ] {
        assert_eq!(tighten_markdown(source), source);
    }
    assert_eq!(tighten_markdown("a\n\n\n\nb"), "a\n\nb");
}

#[test]
fn sequential_requests_fence_edit_confirm_cancel_and_legacy_modal_close() {
    let (mut shell, mut surface, _) = setup(10);
    shell.prefill_editor("parent draft".into());
    let original = shell.extension_editor_snapshot();
    let handler = accepting(&shell);
    shell.begin_tool_input("First fixture request", false);
    let first = tern_prompt::focus(&shell.state.borrow()).unwrap();
    assert!(handler(edit(first.clone(), "🦀雪")).is_none());
    surface.flush(&shell.state).unwrap();
    assert_eq!(surface.sent.focus.as_deref(), Some(first.as_str()));
    assert_eq!(shell.end_tool_input().as_deref(), Some("🦀雪"));
    shell.begin_tool_input("Second fixture request", false);
    let second = tern_prompt::focus(&shell.state.borrow()).unwrap();
    assert_ne!(first, second);
    handler(edit(first.clone(), "stale"));
    assert!(handler(action(first.replace(".editor", ".confirm"), "confirm")).is_none());
    assert!(handler(action(first.replace(".editor", ".cancel"), "cancel")).is_none());
    assert!(handler(action(
        format!("modal.{}", shell.state.borrow().panel_epoch),
        "close"
    ))
    .is_none());
    handler(edit(second.clone(), "current"));
    let cancel = handler(action(second.replace(".editor", ".cancel"), "cancel"));
    assert!(matches!(cancel, Some(InputEvent::Key(key)) if key.code == KeyCode::Esc));
    assert_eq!(shell.end_tool_input().as_deref(), Some("current"));
    let restored = shell.extension_editor_snapshot();
    assert_eq!(restored.text, original.text);
    assert_eq!(restored.cursor, original.cursor);
    assert_eq!(restored.revision, original.revision);
}

#[test]
fn secret_scene_has_no_answer_editor_submit_or_transport_value() {
    let (mut shell, mut surface, output) = setup(8);
    shell.test_set_keybindings(
        crate::tui::keymap::keybindings::KeybindingsManager::with_platform(
            "linux",
            false,
            std::collections::BTreeMap::from([
                ("tui.select.cancel".into(), vec!["n".into()]),
                ("tui.select.confirm".into(), Vec::new()),
            ]),
        ),
    );
    let handler = accepting(&shell);
    shell.begin_tool_input("Fixture secret — no real credential", true);
    let epoch = shell.state.borrow().tool_input_epoch;
    let secret = "PRIVATE_FIXTURE_BYTES_NOT_A_REAL_SECRET";
    let mut host = crate::tui::pickers::SecretInputBuffer::default();
    host.extend_paste(secret);
    handler(edit(format!("prompt.{epoch}.editor"), secret));
    assert!(handler(action(format!("prompt.{epoch}.confirm"), "confirm")).is_none());
    surface.flush(&shell.state).unwrap();
    assert!(surface.sent.focus.is_none());
    walk(&surface.sent.layer, &mut |node| {
        assert!(!matches!(node.k, Kind::Editor | Kind::Input))
    });
    let frames = serde_json::to_string(&output.messages("f")).unwrap();
    assert!(!frames.contains(secret));
    assert!(output.blobs().is_empty());
    assert!(
        matches!(handler(action(format!("prompt.{epoch}.cancel"), "cancel")),
        Some(InputEvent::Key(key)) if key.code == KeyCode::Esc)
    );
    assert_eq!(host.take(), secret.as_bytes());
    shell.end_tool_input();
    assert!(handler(action(format!("prompt.{epoch}.cancel"), "cancel")).is_none());
}

#[test]
fn report_close_cannot_cancel_a_later_report_or_temporary_request() {
    let (mut shell, mut surface, _) = setup(8);
    let handler = accepting(&shell);
    shell.show_report_text("First", "Fixture report", "first body".into());
    surface.flush(&shell.state).unwrap();
    let first = surface.sent.layer[0].id.clone();
    shell.close_overlay();
    shell.show_report_text("Second", "Fixture report", "second body".into());
    surface.flush(&shell.state).unwrap();
    let second = surface.sent.layer[0].id.clone();
    assert_ne!(first, second);
    assert!(handler(action(first, "close")).is_none());
    assert!(handler(action(second.clone(), "close")).is_some());
    shell.begin_tool_input("Exclusive request", false);
    assert!(handler(action(second, "close")).is_none());
}

#[test]
fn catalogue_refresh_rejects_stale_ordinal_selection_and_controls() {
    let (mut shell, _, _) = setup(8);
    choices(&mut shell);
    let handler = accepting(&shell);
    let first = tern_picker::id(&shell.state.borrow());
    if let Some(Panel::SelectList {
        items,
        descriptions,
        ..
    }) = shell.state.borrow_mut().panel.as_mut()
    {
        items[1] = "Different fixture decision".into();
        descriptions[1] = Some("Different scope".into());
    }
    let current = tern_picker::id(&shell.state.borrow());
    assert_ne!(first, current);
    assert!(handler(Incoming::Event(Event::Activate {
        sf: SURFACE.into(),
        id: first.clone(),
        item: "0".into()
    }))
    .is_none());
    assert!(handler(action(format!("{first}.confirm"), "confirm")).is_none());
    assert!(matches!(
        shell.state.borrow().panel,
        Some(Panel::SelectList { selected: 1, .. })
    ));
}

#[test]
fn native_consent_adds_no_positive_pointer_authority_and_retains_host_ack_gate() {
    let (mut shell, mut surface, _) = setup(8);
    choices(&mut shell);
    let handler = accepting(&shell);
    surface.flush(&shell.state).unwrap();
    let panel = tern_picker::id(&shell.state.borrow());
    assert!(handler(action(format!("{panel}.confirm"), "confirm")).is_none());
    assert!(handler(Incoming::Event(Event::Activate {
        sf: SURFACE.into(),
        id: panel.clone(),
        item: format!("{panel}.item.0")
    }))
    .is_none());
    assert!(handler(Incoming::Event(Event::Select {
        sf: SURFACE.into(),
        id: panel.clone(),
        item: format!("{panel}.item.0")
    }))
    .is_none());
    assert!(matches!(
        shell.state.borrow().panel,
        Some(Panel::SelectList { selected: 1, .. })
    ));
    let confirm = InputEvent::Key(crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(shell.panel_input(&confirm).is_none());
    ack(&shell, 1);
    surface.flush(&shell.state).unwrap();
    assert!(shell.state.borrow().painted_panel.is_some());
    let result = shell.panel_input(&confirm).unwrap();
    assert!(matches!(
        result.0,
        crate::tui::view::PanelResult::Confirm(1)
    ));
    assert!(handler(action(format!("{panel}.confirm"), "confirm")).is_none());
}

#[test]
fn native_scroll_is_advertised_credit_owned_and_retained_until_write() {
    let (mut shell, mut surface, output) = setup(1);
    accepting(&shell);
    surface.flush(&shell.state).unwrap();
    shell.scroll(-1);
    assert!(
        shell.state.native().lock().unwrap().scroll.is_empty(),
        "missing feature is not optimistic"
    );
    let hello = json!({"r":"hello","v":1,"term":"test","kinds":TSP_KINDS,"credits":1,"features":["scroll"]});
    shell
        .state
        .native()
        .lock()
        .unwrap()
        .messages
        .push_back(octet_tern::frame::decode_body("r", &hello.to_string()).unwrap());
    surface.flush(&shell.state).unwrap();
    shell.scroll(-1);
    shell.scroll_lines(2);
    shell.jump_to_tail();
    surface.flush(&shell.state).unwrap();
    assert_eq!(
        output.messages("f").len(),
        1,
        "credit admission owns scroll too"
    );
    assert_eq!(shell.state.native().lock().unwrap().scroll.len(), 4);
    ack(&shell, 1);
    surface.flush(&shell.state).unwrap();
    let ops = output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|op| op[0] == "scroll")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        ops,
        vec![
            json!(["scroll", "main", "page-up"]),
            json!(["scroll", "main", "line-down"]),
            json!(["scroll", "main", "line-down"]),
            json!(["scroll", "main", "end"])
        ]
    );
    assert!(shell.state.native().lock().unwrap().scroll.is_empty());
}

fn remote(
    shell: &mut InteractiveShell,
    placement: octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement,
    text: &str,
) {
    use crate::extensions::remote_ui::{MountView, Projection};
    let size = shell.terminal_dimensions();
    let mut components = crate::tui::extension_components::ExtensionComponentSurface::default();
    components.register_component("fixture.remote").unwrap();
    components
        .store_render("fixture.remote", size.0, vec![text.into()])
        .unwrap();
    shell.set_remote_ui(Projection {
        components: Arc::new(components),
        mounts: vec![MountView {
            id: "fixture.remote".into(),
            title: "Fixture remote owner".into(),
            placement,
            columns: size.0,
            rows: size.1,
            mouse_capture: false,
        }],
    });
}

#[test]
fn remote_editor_and_fullscreen_do_not_admit_hidden_composer_actions() {
    use octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement as Placement;
    for placement in [Placement::Editor, Placement::Fullscreen] {
        let (mut shell, mut surface, _) = setup(8);
        shell.prefill_editor("saved draft".into());
        let handler = accepting(&shell);
        remote(&mut shell, placement, "Fixture remote content");
        surface.flush(&shell.state).unwrap();
        assert!(!crate::tui::view::tern::editor_focused(
            &shell.state.borrow()
        ));
        assert!(find_node(&surface.sent.dock, "composer.editor").is_none());
        handler(edit("composer.editor".into(), "stale"));
        for (id, act) in [
            ("composer.send", "send"),
            ("composer.model", "model"),
            ("composer.effort", "effort"),
        ] {
            assert!(handler(action(id.into(), act)).is_none());
        }
        assert_eq!(shell.pending(), "saved draft");
        shell.set_remote_ui(crate::extensions::remote_ui::Projection::default());
        surface.flush(&shell.state).unwrap();
        assert_eq!(surface.sent.focus.as_deref(), Some("composer.editor"));
        assert_eq!(shell.pending(), "saved draft");
    }
}

#[test]
fn remote_header_updates_participate_in_main_retained_cache() {
    use octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement as Placement;
    let (mut shell, mut surface, _) = setup(8);
    remote(&mut shell, Placement::Header, "First fixture header");
    surface.flush(&shell.state).unwrap();
    let first = find_node(&surface.sent.main, "remote.header")
        .unwrap()
        .clone();
    remote(&mut shell, Placement::Header, "Second fixture header");
    surface.flush(&shell.state).unwrap();
    let next = find_node(&surface.sent.main, "remote.header").unwrap();
    assert_ne!(first, *next);
    assert!(next.p.as_ref().unwrap().as_map()["text"]
        .as_str()
        .unwrap()
        .contains("Second fixture header"));
}

/// Replayable compiled-renderer fixtures, not executed tools/provider conversations.
#[test]
#[ignore = "explicit local native-capture fixture export; requires OCTET_RAIL_CAPTURE_DIR"]
fn export_native_renderer_fixtures() {
    use crate::tui::view::{summarize_tool, ToolPanel};
    use octet_ai::ToolCallId;
    let root = std::path::PathBuf::from(
        std::env::var_os("OCTET_RAIL_CAPTURE_DIR").expect("private fixture output directory"),
    );
    std::fs::create_dir_all(&root).unwrap();
    for name in [
        "conversation",
        "commands-quiet",
        "commands-verbose",
        "codemode-quiet",
        "codemode-verbose",
        "consent",
        "ordinary",
        "secret",
        "menu",
        "thinking",
    ] {
        let (mut shell, mut surface, output) = setup(128);
        shell.set_identity("fixture", "fixture-model", "medium");
        shell.prefill_editor("Native renderer fixture — not a provider conversation".into());
        shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
        shell.state.borrow_mut().push_block(TranscriptBlock::User {
            text: "Fixture: inspect the native RAIL layout".into(),
            model_lab: None,
            prompt_color: None,
            persisted: true,
        });
        let mut reply = AssistantBlock::streaming("This is **compiled-renderer fixture content**, not a live answer.\n\n- Retained Markdown\n- Native layout\n\n$E=mc^2$");
        reply.finished = true;
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Assistant(Box::new(reply)));
        match name {
            "commands-quiet" | "commands-verbose" => {
                let command = "printf 'native fixture\\n'\nprintf 'second fixture line\\n'\n# This multiline command is NOT executed";
                let args = json!({"command": command});
                let mut panel = ToolPanel::new(ToolCallId("fixture-command".into()), "bash".into(), args.to_string(), summarize_tool("bash", &args), "CAPTURED FIXTURE OUTPUT — not executed\nsecond fixture line".into(), true, false, None, None);
                panel.duration = Some(Duration::from_millis(1200));
                shell.state.borrow_mut().push_block(TranscriptBlock::Tool(Box::new(panel)));
                shell.state.borrow_mut().verbose_tools = name == "commands-verbose";
            }
            "codemode-quiet" | "codemode-verbose" => {
                shell.state.borrow_mut().workspace = Some("/tmp/native-fixture".into());
                shell.state.borrow_mut().context_estimate = Some((24000, 200000));
                let read_args = json!({"path":"src/main.rs"});
                let mut read = ToolPanel::new(
                    ToolCallId("fixture-read".into()), "read".into(), read_args.to_string(),
                    summarize_tool("read", &read_args), "File fixture — not read".into(),
                    true, false, None, None,
                );
                read.duration = Some(Duration::from_millis(2));
                shell.state.borrow_mut().push_block(TranscriptBlock::Tool(Box::new(read)));
                let args = json!({"code":"const results = await Promise.allSettled([\n  tools.read({path: 'README.md'}),\n  tools.read({path: 'Cargo.toml'}),\n]);\nreturn results.map(r => r.status);"});
                let mut panel = ToolPanel::new(
                    ToolCallId("fixture-codemode".into()), "codemode".into(), args.to_string(),
                    summarize_tool("codemode", &args),
                    "Script completed\nWall time 0.1 seconds\nOutput:\n[\"fulfilled\", \"fulfilled\"]\n\nFixture only — no tools executed\nAdditional retained output".into(),
                    true, false, None, None,
                );
                panel.duration = Some(Duration::from_millis(120));
                shell.state.borrow_mut().push_block(TranscriptBlock::Tool(Box::new(panel)));
                shell.state.borrow_mut().verbose_tools = name == "codemode-verbose";
                let bash_args = json!({"command":"printf 'fixture only\\n'"});
                let mut bash = ToolPanel::new(
                    ToolCallId("fixture-bash".into()), "bash".into(), bash_args.to_string(),
                    summarize_tool("bash", &bash_args), "Bash fixture — not executed".into(),
                    true, false, None, None,
                );
                bash.duration = Some(Duration::from_millis(42));
                shell.state.borrow_mut().push_block(TranscriptBlock::Tool(Box::new(bash)));
                shell.queue_follow_up(super::super::super::ComposedInput::from_text("Review the two results\nthen continue".into()));
            }
            "consent" => choices(&mut shell),
            "ordinary" => shell.begin_tool_input("Fixture ordinary request\nComplete prompt remains visible", false),
            "secret" => shell.begin_tool_input("Fixture secret request — no real credential", true),
            "menu" => shell.open_panel(Panel::SelectList { surface: OrdinarySurfaceMetadata::with_purpose("Fixture › nested menu", "Source-owned menu projection"), items: vec!["Inspect fixture".into(), "A deliberately long full selected fixture label that should wrap below the list".into()], descriptions: vec![Some("No action runs".into());2], selected:1, filter:String::new(), action:PanelAction::SelectModel(vec![octet_ai::ModelId("fixture-a".into()),octet_ai::ModelId("fixture-b".into())]) }),
            "thinking" => {
                let mut trace = AssistantBlock::streaming_reasoning("# Fixture reasoning\n\nThis is an authored trace fixture.\n\n```rust\n\n\nlet fixture = true;\n```");
                trace.finished = true;
                trace.reasoning_expanded = true;
                shell.state.borrow_mut().push_block(TranscriptBlock::Reasoning(Box::new(trace)));
            },
            _ => {},
        }
        surface.flush(&shell.state).unwrap();
        std::fs::write(
            root.join(format!("{name}.tsp")),
            output.0.lock().unwrap().join(""),
        )
        .unwrap();
        std::fs::write(
            root.join(format!("{name}.json")),
            serde_json::to_string_pretty(&[
                &surface.sent.main,
                &surface.sent.dock,
                &surface.sent.layer,
            ])
            .unwrap(),
        )
        .unwrap();
    }
}

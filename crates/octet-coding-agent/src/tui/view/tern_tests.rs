//! Protocol/tree regressions use an in-memory native sink, never a desktop.

use super::super::{AssistantBlock, InteractiveShell, OrdinarySurfaceMetadata, Panel, PanelAction};
use super::*;
use octet_tern::wire::TSP_KINDS;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<String>>>);
impl io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .push(String::from_utf8(bytes.to_vec()).unwrap());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Output {
    fn messages(&self, verb: &str) -> Vec<serde_json::Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|wire| {
                let raw = octet_tern::frame::split(wire)?;
                (raw.verb == verb).then(|| serde_json::from_str(&raw.body).unwrap())
            })
            .collect()
    }
    fn blobs(&self) -> Vec<octet_tern::frame::Raw> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|wire| octet_tern::frame::split(wire).filter(|raw| raw.verb == "b"))
            .collect()
    }
    fn last_frame(&self) -> serde_json::Value {
        self.messages("f").pop().unwrap()
    }
}

fn hello(dark: bool, credits: u32) -> Incoming {
    octet_tern::frame::decode_body(
        "r",
        &json!({"r":"hello","v":1,"term":"test","kinds":TSP_KINDS,"credits":credits,"dark":dark})
            .to_string(),
    )
    .unwrap()
}
fn setup(credits: u32) -> (InteractiveShell, TernSurface, Output) {
    let shell = InteractiveShell::test_shell();
    {
        let mut state = shell.state.borrow_mut();
        state.startup_pending = false;
        state.render_threaded = true;
    }
    let output = Output::default();
    let client = TernClient::with_writer("test", None, &["edit"], output.clone()).unwrap();
    let mut surface = TernSurface::with_client(client);
    surface.observe(&hello(true, credits)).unwrap();
    (shell, surface, output)
}
fn ack(shell: &InteractiveShell, sequence: u64) {
    shell
        .state
        .native()
        .lock()
        .unwrap()
        .messages
        .push_back(Incoming::Event(Event::Ack {
            sf: SURFACE.into(),
            s: sequence,
        }));
}
fn walk(nodes: &[Node], visit: &mut impl FnMut(&Node)) {
    for node in nodes {
        visit(node);
        walk(node.c.as_deref().unwrap_or_default(), visit);
    }
}

#[test]
fn composer_uses_native_layout_hooks_with_octet_controls_and_no_rows() {
    let (shell, mut surface, _) = setup(2);
    shell.state.borrow_mut().editor.set_text("/");
    shell.state.borrow_mut().context_estimate = Some((1234, 131072));
    surface.flush(&shell.state).unwrap();
    let projection = &surface.sent;
    let composer = find_node(&projection.dock, "composer").unwrap();
    assert_eq!(composer.k, Kind::Col);
    let props = composer.p.as_ref().unwrap().as_map();
    for removed in ["role", "head", "frame", "border"] {
        assert!(
            !props.contains_key(removed),
            "borderless composer: {removed}"
        );
    }
    assert_eq!(composer.c.as_ref().unwrap()[0].id, "composer.rule");
    assert_eq!(
        find_node(&projection.dock, "composer.rule").unwrap().k,
        Kind::Rule
    );
    walk(&projection.dock, &mut |node| {
        assert_ne!(
            node.p.as_ref().and_then(|props| props.as_map().get("role")),
            Some(&json!("omp.editor"))
        );
    });
    assert!(find_node(&projection.dock, "composer.context").is_some());
    assert!(projection.layer.iter().any(|node| node.k == Kind::Overlay));
    walk(&projection.dock, &mut |node| {
        assert_ne!(node.k, Kind::Badge);
        assert_ne!(node.k, Kind::Rows);
    });
    walk(&projection.layer, &mut |node| {
        assert_ne!(node.k, Kind::Rows)
    });
    shell.state.borrow_mut().slash_popup_dismissed = true;
    surface.flush(&shell.state).unwrap();
    assert!(
        surface.sent.layer.is_empty(),
        "Esc must not open a root-directory popup"
    );
}

#[test]
fn streaming_materializes_source_and_only_patches_the_retained_leaf() {
    let (shell, mut surface, output) = setup(2);
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::streaming("hello"),
        )));
    surface.flush(&shell.state).unwrap();
    let identity = shell.state.borrow().transcript_commit_ids[0];
    assert_eq!(
        find_node(&surface.sent.main, &id(identity, "assistant.md"))
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["text"],
        "hello"
    );
    {
        let mut state = shell.state.borrow_mut();
        let TranscriptBlock::Assistant(block) = &mut state.transcript[0] else {
            unreachable!()
        };
        block.append(" 🦀 world");
        state.touch_block(0);
    }
    surface.flush(&shell.state).unwrap();
    let frame = output.last_frame();
    assert!(frame["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op[0] == "set"
            && op[1] == id(identity, "assistant.md")
            && op[2]["text"] == "hello 🦀 world"));
    assert!(!frame["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op[0] == "add" || op[0] == "del"));
}

#[test]
fn assistant_replies_are_unboxed_prose_with_a_direct_retained_markdown_leaf() {
    let (shell, mut surface, _) = setup(2);
    shell.state.borrow_mut().model_display = "Sonnet 4.5".into();
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::streaming("the fix is a checked subtraction"),
        )));
    surface.flush(&shell.state).unwrap();
    let identity = shell.state.borrow().transcript_commit_ids[0];
    let group = find_node(&surface.sent.main, &id(identity, "assistant.group")).unwrap();
    assert_eq!(group.p.as_ref().unwrap().as_map()["role"], "omp.assistant");
    let assistant = find_node(&surface.sent.main, &id(identity, "assistant")).unwrap();
    assert_eq!(assistant.k, Kind::Col);
    assert_eq!(group.c.as_ref().unwrap()[0].id, assistant.id);
    let props = assistant.p.as_ref().unwrap().as_map();
    assert_eq!(props["role"], "omp.assistant");
    for removed in ["head", "frame", "border"] {
        assert!(!props.contains_key(removed), "unboxed reply: {removed}");
    }
    assert_eq!(assistant.c.as_ref().unwrap().len(), 1);
    assert_eq!(
        assistant.c.as_ref().unwrap()[0].id,
        id(identity, "assistant.md")
    );
    assert!(find_node(&surface.sent.main, &id(identity, "assistant.body")).is_none());
    walk(std::slice::from_ref(group), &mut |node| {
        assert_ne!(node.k, Kind::Card)
    });
    assert!(!serde_json::to_string(group).unwrap().contains("Sonnet 4.5"));
    // Streaming still lives on the direct Markdown leaf.
    let body = find_node(&surface.sent.main, &id(identity, "assistant.md")).unwrap();
    assert_eq!(body.k, Kind::Md);
    assert_eq!(body.p.as_ref().unwrap().as_map()["stream"], true);
}

#[test]
fn reasoning_projection_keeps_native_layout_and_tightened_markdown() {
    let (shell, mut surface, _) = setup(2);
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::streaming_reasoning("one\n\n\n\ntwo\n\n```rust\n\n\nlet x = 1;\n```"),
        )));
    surface.flush(&shell.state).unwrap();
    let identity = shell.state.borrow().transcript_commit_ids[0];
    let group = find_node(&surface.sent.main, &id(identity, "reasoning.group")).unwrap();
    assert_eq!(group.p.as_ref().unwrap().as_map()["role"], "omp.assistant");
    let section = find_node(&surface.sent.main, &id(identity, "reasoning")).unwrap();
    assert_eq!(section.p.as_ref().unwrap().as_map()["role"], "omp.thinking");
    let body = find_node(&surface.sent.main, &id(identity, "reasoning.md")).unwrap();
    assert_eq!(
        body.p.as_ref().unwrap().as_map()["text"],
        "one\n\ntwo\n\n```rust\n\n\nlet x = 1;\n```"
    );
}

#[test]
fn missing_credit_keeps_the_sent_baseline_until_the_latest_state_can_be_sent() {
    let (shell, mut surface, output) = setup(1);
    surface.flush(&shell.state).unwrap();
    shell.state.borrow_mut().editor.set_text("latest");
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 1);
    assert_eq!(
        find_node(&surface.sent.dock, "composer.editor")
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["text"],
        ""
    );
    ack(&shell, 1);
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 2);
    assert_eq!(
        find_node(&surface.sent.dock, "composer.editor")
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["text"],
        "latest"
    );
    surface.credit_blocked = Some(Instant::now() - HELLO_TIMEOUT);
    assert_eq!(
        surface.flush(&shell.state).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn resize_motion_and_appearance_keep_the_surface_and_transcript_identity() {
    let (mut shell, mut surface, output) = setup(2);
    shell.state.borrow_mut().push_block(TranscriptBlock::User {
        text: "historic 🦀".into(),
        model_lab: Some(ModelLab::Anthropic),
        prompt_color: Some("#d97757".into()),
        persisted: true,
    });
    surface.flush(&shell.state).unwrap();
    let user_id = surface.sent.main[0].id.clone();
    ack(&shell, 1);
    surface
        .observe(&Incoming::Event(Event::Resize {
            sf: Some(SURFACE.into()),
            cols: 61,
            cell: None,
            visible: Some(true),
        }))
        .unwrap();
    shell.set_size(61, 19);
    surface
        .observe(&Incoming::Event(Event::Theme { dark: false }))
        .unwrap();
    surface
        .observe(&Incoming::Event(Event::Motion { reduce: true }))
        .unwrap();
    surface.flush(&shell.state).unwrap();
    assert_eq!(surface.sent.main[0].id, user_id);
    assert_eq!(output.messages("o").len(), 1);
    assert!(output.messages("x").is_empty());
    assert_eq!(
        surface.owner.state.theme.background(),
        crate::tui::theme::TerminalBackground::Light
    );
    assert!(surface.client.reduce_motion());
    // The native card is laid out by Tern, so a resize may send no transcript
    // ops at all; whatever follows the first frame must never rebuild it.
    assert!(output
        .messages("f")
        .iter()
        .skip(1)
        .all(|frame| !frame["ops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| (op[0] == "add" || op[0] == "del") && op[1] == user_id)));
}

#[test]
fn resize_without_visibility_transition_reasserts_current_owner_focus() {
    for temporary_owner in [false, true] {
        let (mut shell, mut surface, output) = setup(2);
        if temporary_owner {
            shell.begin_tool_input("Owned input", false);
        }
        surface.flush(&shell.state).unwrap();
        let focus = surface.sent.focus.clone();
        ack(&shell, 1);
        surface
            .observe(&Incoming::Event(Event::Resize {
                sf: Some(SURFACE.into()),
                cols: 120,
                cell: None,
                visible: Some(true),
            }))
            .unwrap();
        surface.flush(&shell.state).unwrap();
        assert_eq!(output.messages("f").len(), 2);
        assert!(output.last_frame()["ops"]
            .as_array()
            .unwrap()
            .contains(&json!(["focus", focus])));
        assert_eq!(output.messages("o").len(), 1);
    }
}

#[test]
fn model_switch_preserves_stored_prompt_color_instead_of_using_the_new_accent() {
    let (shell, mut surface, output) = setup(2);
    shell.state.borrow_mut().push_block(TranscriptBlock::User {
        text: "history".into(),
        model_lab: Some(ModelLab::Anthropic),
        prompt_color: Some("#d97757".into()),
        persisted: true,
    });
    surface.flush(&shell.state).unwrap();
    let history = surface.sent.main.clone();
    ack(&shell, 1);
    shell.state.borrow_mut().model_lab = Some(ModelLab::OpenAi);
    surface.flush(&shell.state).unwrap();
    assert_eq!(history, surface.sent.main);
    assert_eq!(output.messages("t").len(), 2);
}

#[test]
fn eviction_reopens_explicitly_and_replays_regions_without_reusing_old_credit() {
    let (shell, mut surface, output) = setup(1);
    surface.flush(&shell.state).unwrap();
    surface
        .observe(&Incoming::Event(Event::Gone {
            sf: Some(SURFACE.into()),
            ids: vec!["main".into()],
        }))
        .unwrap();
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("o").len(), 2);
    assert_eq!(output.last_frame()["s"], 1);
    assert!(output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op[0] == "add" && op[1] == "dock"));
}

#[test]
fn approval_consent_waits_for_ack_and_never_survives_an_unpainted_selection_change() {
    let (mut shell, mut surface, _) = setup(2);
    shell.set_size(100, 24);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Approve exact effect?"),
        items: vec!["Deny".into(), "Approve".into()],
        descriptions: vec![Some("effect sha256: 1234".into()), None],
        selected: 1,
        filter: String::new(),
        action: PanelAction::Confirmation,
    });
    surface.flush(&shell.state).unwrap();
    assert!(shell.state.borrow().painted_panel.is_none());
    ack(&shell, 1);
    surface.present(&shell.state).unwrap();
    assert!(shell.state.borrow().painted_panel.is_some());
    if let Some(Panel::SelectList { selected, .. }) = shell.state.borrow_mut().panel.as_mut() {
        *selected = 0;
    }
    let key = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(shell.panel_input(&key).is_none());
}

#[test]
fn composer_edits_reuse_the_history_allocation_without_touching_its_nodes() {
    let (shell, mut surface, output) = setup(2);
    shell.state.borrow_mut().push_block(TranscriptBlock::User {
        text: "long history".repeat(1000),
        model_lab: None,
        prompt_color: None,
        persisted: true,
    });
    surface.flush(&shell.state).unwrap();
    let history = surface.sent.main.as_ptr();
    shell.state.borrow_mut().editor.set_text("fast draft");
    surface.flush(&shell.state).unwrap();
    assert_eq!(surface.sent.main.as_ptr(), history);
    assert!(output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .all(|op| !op[1].as_str().is_some_and(|id| id.starts_with('t'))));
}

const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0,
    0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5, 1, 1,
    39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[test]
fn validated_images_upload_once_with_payload_free_nodes_and_policy_fallbacks() {
    use super::super::{summarize_tool, ToolPanel};
    use crate::hydrate::ToolResultImage;
    use octet_ai::ToolCallId;
    use sexy_tui_rs::images::TerminalImage;
    for enabled in [false, true] {
        for supported in [false, true] {
            let (shell, mut surface, output) = setup(2);
            if !supported {
                let hello = json!({"r":"hello","v":1,"term":"test","kinds":TSP_KINDS.iter().filter(|kind| **kind != "image").collect::<Vec<_>>(),"credits":2});
                surface
                    .observe(&octet_tern::frame::decode_body("r", &hello.to_string()).unwrap())
                    .unwrap();
            }
            let args = json!({"path":"pixel.png"});
            let mut panel = ToolPanel::new(
                ToolCallId("image".into()),
                "read".into(),
                args.to_string(),
                summarize_tool("read", &args),
                String::new(),
                true,
                false,
                None,
                None,
            );
            let payload = Arc::new(TerminalImage::from_slice(PNG).unwrap());
            panel.images = vec![
                ToolResultImage::Ready {
                    image: payload.clone(),
                    id: None,
                },
                ToolResultImage::Ready {
                    image: payload,
                    id: None,
                },
            ];
            panel.image_rendering.enabled = enabled;
            shell.state.borrow_mut().update_image_rendering(
                enabled,
                sexy_tui_rs::images::ImageCapabilities::forced(None, None),
            );
            shell
                .state
                .borrow_mut()
                .push_block(TranscriptBlock::Tool(Box::new(panel)));
            surface.flush(&shell.state).unwrap();
            let mut image_nodes = 0;
            walk(&surface.sent.main, &mut |node| {
                if node.k == Kind::Image {
                    image_nodes += 1;
                }
            });
            assert_eq!(image_nodes, if enabled && supported { 2 } else { 0 });
            let blobs = output.blobs();
            assert_eq!(blobs.len(), usize::from(enabled && supported));
            if let Some(blob) = blobs.first() {
                use base64::Engine;
                assert_eq!(
                    base64::engine::general_purpose::STANDARD
                        .decode(&blob.body)
                        .unwrap(),
                    PNG
                );
                assert_eq!(blob.params["mime"], "image/png");
                assert!(surface.dump().iter().all(|row| !row.contains(&blob.body)));
            }
            ack(&shell, 1);
            shell
                .state
                .borrow_mut()
                .editor
                .set_text("no repeated upload");
            surface.flush(&shell.state).unwrap();
            assert_eq!(output.blobs().len(), blobs.len());
        }
    }
}

#[test]
fn completed_tool_uses_native_diff_and_retains_disclosure_gestures() {
    use super::super::{summarize_tool, ToolPanel};
    use octet_ai::ToolCallId;
    let (shell, mut surface, output) = setup(2);
    let args = json!({"path":"src/lib.rs"});
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("edit".into()),
            "edit".into(),
            args.to_string(),
            summarize_tool("edit", &args),
            "ok modified=1\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n"
                .into(),
            true,
            false,
            None,
            None,
        ))));
    surface.flush(&shell.state).unwrap();
    let identity = shell.state.borrow().transcript_commit_ids[0];
    assert_eq!(
        find_node(&surface.sent.main, &id(identity, "diff"))
            .unwrap()
            .k,
        Kind::Diff
    );
    assert_eq!(
        find_node(&surface.sent.main, &id(identity, "tool"))
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["collapsed"],
        true
    );
    ack(&shell, 1);
    surface
        .observe(&Incoming::Event(Event::Toggle {
            sf: SURFACE.into(),
            id: id(identity, "tool"),
            collapsed: false,
            key: None,
        }))
        .unwrap();
    surface.flush(&shell.state).unwrap();
    assert!(output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op[0] == "set" && op[1] == id(identity, "tool") && op[2]["collapsed"] == false));
    assert!(find_node(&surface.sent.main, &id(identity, "diff")).is_some());
}

#[test]
fn coalesced_drafts_keep_frontend_completion_revisions_and_pointer_selection() {
    let (mut shell, mut surface, _) = setup(2);
    for text in ["@", "@a", "@ab", "@abc"] {
        shell.state.borrow_mut().editor.set_text(text);
    }
    let snapshot = shell.extension_editor_snapshot();
    assert!(shell.set_extension_autocomplete(
        &snapshot,
        "@abc".into(),
        ["alpha", "beta"]
            .into_iter()
            .map(|value| super::super::ShellAutocompleteItem {
                value: value.into(),
                label: value.into(),
                description: None
            })
            .collect()
    ));
    surface.flush(&shell.state).unwrap();
    assert_ne!(surface.owner.state.editor.revision(), snapshot.revision);
    let completion =
        super::super::tern_completion::Completion::capture(&shell.state.borrow()).unwrap();
    assert!(find_node(&surface.sent.layer, &completion.id).is_some());
    shell.state.native().lock().unwrap().accepting_input = true;
    let handler = shell.tern_input_handler();
    handler(Incoming::Event(Event::Select {
        sf: SURFACE.into(),
        id: completion.id.clone(),
        item: completion.item_id(1),
    }));
    assert!(shell.accept_extension_autocomplete());
    assert_eq!(shell.pending(), "beta");
}

#[test]
fn focus_return_during_materialization_is_not_consumed_without_focus() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FocusWriter {
        output: Output,
        state: SharedState,
        armed: Arc<AtomicBool>,
    }
    impl io::Write for FocusWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.armed.swap(false, Ordering::SeqCst) {
                self.state.native().lock().unwrap().focus_resync += 1;
            }
            self.output.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().startup_pending = false;
    shell.state.borrow_mut().render_threaded = true;
    let output = Output::default();
    let armed = Arc::new(AtomicBool::new(false));
    let client = TernClient::with_writer(
        "test",
        None,
        &["edit"],
        FocusWriter {
            output: output.clone(),
            state: shell.state.clone(),
            armed: armed.clone(),
        },
    )
    .unwrap();
    let mut surface = TernSurface::with_client(client);
    surface.observe(&hello(true, 2)).unwrap();
    surface.flush(&shell.state).unwrap();
    ack(&shell, 1);
    // A palette write occurs after the frontend snapshot was captured. Deliver
    // focus in that gap, deterministically, without a timing-dependent thread.
    shell.state.borrow_mut().theme_epoch += 1;
    armed.store(true, Ordering::SeqCst);
    surface.flush(&shell.state).unwrap();
    assert!(!armed.load(Ordering::SeqCst));
    assert!(output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op == &json!(["focus", "composer.editor"])));
}

#[test]
fn hidden_pane_with_available_credit_defers_edits_until_visible() {
    let (shell, mut surface, output) = setup(2);
    surface.flush(&shell.state).unwrap();
    surface
        .observe(&Incoming::Event(Event::Visible {
            sf: Some(SURFACE.into()),
            visible: false,
        }))
        .unwrap();
    shell.state.borrow_mut().editor.set_text("hidden draft 🦀");
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 1);
    assert!(surface.client.has_credit(SURFACE));
    surface
        .observe(&Incoming::Event(Event::Visible {
            sf: Some(SURFACE.into()),
            visible: true,
        }))
        .unwrap();
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 2);
    assert_eq!(
        find_node(&surface.sent.dock, "composer.editor")
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["text"],
        "hidden draft 🦀"
    );
    assert!(output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op == &json!(["focus", "composer.editor"])));
}

#[test]
fn hidden_pane_keeps_its_place_and_reasserts_focus_on_return() {
    let (shell, mut surface, output) = setup(1);
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 1);
    surface
        .observe(&Incoming::Event(Event::Visible {
            sf: Some(SURFACE.into()),
            visible: false,
        }))
        .unwrap();
    // Hidden with credit exhausted: suspension, never a renderer failure.
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 1);
    ack(&shell, 1);
    surface
        .observe(&Incoming::Event(Event::Visible {
            sf: Some(SURFACE.into()),
            visible: true,
        }))
        .unwrap();
    surface.flush(&shell.state).unwrap();
    let frame = output.last_frame();
    assert!(frame["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op[0] == "focus" && op[1] == "composer.editor"));
}

#[test]
fn bash_is_summary_only_from_first_progress_through_completion() {
    use super::super::{summarize_tool, ToolPanel};
    use octet_ai::ToolCallId;
    let (shell, mut surface, output) = setup(2);
    let command = format!("printf '%s\\n' '{}'\necho done", "λ🙂 ".repeat(100));
    let args = json!({"command":command});
    let index = shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("bash1".into()),
            "bash".into(),
            args.to_string(),
            summarize_tool("bash", &args),
            "line1\nline2\nline3".into(),
            false,
            false,
            None,
            None,
        ))));
    surface.flush(&shell.state).unwrap();
    let identity = shell.state.borrow().transcript_commit_ids[index];
    // Running and finished command output use the same disclosure policy.
    assert!(find_node(&surface.sent.main, &id(identity, "out")).is_none());
    let tool = find_node(&surface.sent.main, &id(identity, "tool")).unwrap();
    let props = tool.p.as_ref().unwrap().as_map();
    assert!(!props.contains_key("collapsed"));
    assert!(!props.contains_key("collapsible"));
    assert_eq!(tool.k, Kind::Row);
    assert_eq!(props["role"], "octet.command");
    assert!(props.get("target").is_none());
    let command_leaf = find_node(&surface.sent.main, &id(identity, "command"))
        .unwrap()
        .clone();
    assert_eq!(command_leaf.k, Kind::Code);
    let command_props = command_leaf.p.as_ref().unwrap().as_map();
    assert_eq!(
        command_props["text"].as_str().unwrap().as_bytes(),
        command.as_bytes()
    );
    assert_eq!(command_props["wrap"], true);
    assert_eq!(command_props["numbers"], false);
    assert_eq!(
        find_node(&surface.sent.main, &id(identity, "command.label"))
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["spans"][1]["t"],
        " · running"
    );
    assert!(!output.last_frame().to_string().contains("line1"));
    ack(&shell, 1);
    {
        let mut state = shell.state.borrow_mut();
        if let TranscriptBlock::Tool(panel) = &mut state.transcript[index] {
            panel.finished = true;
            panel.duration = Some(Duration::from_millis(1200));
            panel.output = "exit=0 duration=1.2s\n--- stdout ---\nline1\nline2\nline3\n".into();
        }
        state.touch_block(index);
    }
    surface.flush(&shell.state).unwrap();
    // Finished and non-verbose: collapsed to the command summary, with no
    // output child mounted — the completion must not paint one expanded
    // frame before the terminal hides it.
    let tool = find_node(&surface.sent.main, &id(identity, "tool")).unwrap();
    assert!(!tool.p.as_ref().unwrap().as_map().contains_key("collapsed"));
    assert!(find_node(&surface.sent.main, &id(identity, "out")).is_none());
    let ops = output.last_frame()["ops"].clone();
    let ops = ops.as_array().unwrap();
    assert!(
        !ops.iter().any(|op| op[1] == id(identity, "out")),
        "collapsed output must never have been mounted: {ops:?}"
    );
    let label = find_node(&surface.sent.main, &id(identity, "command.label")).unwrap();
    let spans = &label.p.as_ref().unwrap().as_map()["spans"];
    assert_eq!(spans[1]["t"], " · done");
    assert_eq!(spans[2]["t"], " · 1.2s");
    assert_eq!(
        find_node(&surface.sent.main, &id(identity, "command")).unwrap(),
        &command_leaf
    );
    assert!(
        !ops.iter().any(|op| op[1] == id(identity, "command")),
        "completion retains full command bytes"
    );
    assert!(
        !ops.iter()
            .any(|op| op[0] == "set" && op[1] == id(identity, "out")),
        "completion must not set full output text while collapsing: {ops:?}"
    );
}

#[test]
fn verbose_commands_patch_one_cursorless_text_leaf_and_use_ctrl_o_disclosure() {
    use super::super::{summarize_tool, ToolPanel};
    use octet_ai::ToolCallId;
    for name in ["bash", "exec"] {
        let (shell, mut surface, output) = setup(16);
        shell.state.borrow_mut().verbose_tools = true;
        let args = json!({"command":"git diff"});
        let index = shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId("streaming-command".into()),
                name.into(),
                args.to_string(),
                summarize_tool(name, &args),
                String::new(),
                false,
                false,
                None,
                None,
            ))));
        let identity = shell.state.borrow().transcript_commit_ids[index];
        let chunks = [
            "diff --git a/a b/a",
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new",
        ];
        for (step, text) in chunks.iter().enumerate() {
            {
                let mut state = shell.state.borrow_mut();
                if let TranscriptBlock::Tool(panel) = &mut state.transcript[index] {
                    panel.output = (*text).into();
                }
                state.touch_block(index);
            }
            surface.flush(&shell.state).unwrap();
            let command = find_node(&surface.sent.main, &id(identity, "command")).unwrap();
            assert_eq!(command.k, Kind::Code);
            let command_props = command.p.as_ref().unwrap().as_map();
            assert_eq!(command_props["text"], "git diff");
            assert_eq!(command_props["wrap"], true);
            assert_eq!(command_props["numbers"], false);
            let leaf = find_node(&surface.sent.main, &id(identity, "out")).unwrap();
            assert_eq!(leaf.k, Kind::Text);
            let props = leaf.p.as_ref().unwrap().as_map();
            assert_eq!(props["spans"][0]["t"], *text);
            assert!(props.get("stream").is_none());
            assert!(props.get("cursor").is_none());
            if step > 0 {
                let frame = output.last_frame();
                let ops = frame["ops"].as_array().unwrap();
                assert!(ops
                    .iter()
                    .any(|op| op[0] == "set" && op[1] == id(identity, "out")));
                assert!(!ops
                    .iter()
                    .any(|op| matches!(op[0].as_str(), Some("add" | "del" | "move"))));
            }
        }
        // Collapsing mid-run removes the projection, never the captured source.
        shell.state.borrow_mut().verbose_tools = false;
        surface.flush(&shell.state).unwrap();
        assert!(find_node(&surface.sent.main, &id(identity, "out")).is_none());
        surface
            .observe(&Incoming::Event(Event::Toggle {
                sf: SURFACE.into(),
                id: id(identity, "tool"),
                collapsed: false,
                key: None,
            }))
            .unwrap();
        surface.flush(&shell.state).unwrap();
        // Native toggles cannot mount hidden output or truncate command input.
        assert!(find_node(&surface.sent.main, &id(identity, "out")).is_none());
        shell.state.borrow_mut().verbose_tools = true;
        surface.flush(&shell.state).unwrap();
        assert!(find_node(&surface.sent.main, &id(identity, "out")).is_some());
        {
            let mut state = shell.state.borrow_mut();
            if let TranscriptBlock::Tool(panel) = &mut state.transcript[index] {
                panel.finished = true;
                panel.is_error = true;
                panel.failure_reason = Some("command failed".into());
                panel.output = "final captured failure".into();
            }
            state.touch_block(index);
        }
        surface.flush(&shell.state).unwrap();
        let tool = find_node(&surface.sent.main, &id(identity, "tool")).unwrap();
        assert!(!tool.p.as_ref().unwrap().as_map().contains_key("collapsed"));
        assert_eq!(tool.k, Kind::Row);
        let label = find_node(&surface.sent.main, &id(identity, "command.label")).unwrap();
        let spans = &label.p.as_ref().unwrap().as_map()["spans"];
        assert_eq!(spans[1]["t"], " · error");
        assert_eq!(spans[1]["s"], "error");
        assert_eq!(spans[2]["t"], " · command failed");
        let leaf = find_node(&surface.sent.main, &id(identity, "out")).unwrap();
        assert_eq!(leaf.k, Kind::Text);
        assert_eq!(
            leaf.p.as_ref().unwrap().as_map()["spans"][0]["t"],
            "final captured failure"
        );
        surface
            .observe(&Incoming::Event(Event::Toggle {
                sf: SURFACE.into(),
                id: id(identity, "tool"),
                collapsed: true,
                key: None,
            }))
            .unwrap();
        surface.flush(&shell.state).unwrap();
        assert!(find_node(&surface.sent.main, &id(identity, "out")).is_some());
        assert!(!find_node(&surface.sent.main, &id(identity, "tool"))
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()
            .contains_key("collapsed"));
        shell.state.borrow_mut().verbose_tools = false;
        surface.flush(&shell.state).unwrap();
        assert!(find_node(&surface.sent.main, &id(identity, "out")).is_none());
        let state = shell.state.borrow();
        let TranscriptBlock::Tool(panel) = &state.transcript[index] else {
            panic!("tool fixture")
        };
        assert_eq!(panel.output, "final captured failure");
    }
}

#[test]
fn local_shell_is_summary_only_during_execution_and_after_success_or_failure() {
    use super::super::ShellOutput;
    let (shell, mut surface, _) = setup(2);
    let running = shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Shell(Box::new(ShellOutput {
            id: "s-running".into(),
            command: "cargo test".into(),
            output: "line1".into(),
            exit_code: 0,
            running: true,
        })));
    let done = shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Shell(Box::new(ShellOutput {
            id: "s-done".into(),
            command: "cargo test".into(),
            output: "test result: ok".into(),
            exit_code: 1,
            running: false,
        })));
    surface.flush(&shell.state).unwrap();
    let ids = shell.state.borrow().transcript_commit_ids.clone();
    // Neither running nor failed local shell output is mounted while collapsed.
    assert!(find_node(&surface.sent.main, &id(ids[running], "out")).is_none());
    assert!(!find_node(&surface.sent.main, &id(ids[running], "shell"))
        .unwrap()
        .p
        .as_ref()
        .unwrap()
        .as_map()
        .contains_key("collapsed"));
    // The failure's exit code stays visible in its summary.
    assert!(!find_node(&surface.sent.main, &id(ids[done], "shell"))
        .unwrap()
        .p
        .as_ref()
        .unwrap()
        .as_map()
        .contains_key("collapsed"));
    assert!(find_node(&surface.sent.main, &id(ids[done], "out")).is_none());
    for index in [running, done] {
        let rail = find_node(&surface.sent.main, &id(ids[index], "shell")).unwrap();
        assert_eq!(rail.k, Kind::Row);
        assert!(!rail
            .p
            .as_ref()
            .unwrap()
            .as_map()
            .contains_key("collapsible"));
        let command = find_node(&surface.sent.main, &id(ids[index], "command")).unwrap();
        assert_eq!(command.k, Kind::Code);
        let props = command.p.as_ref().unwrap().as_map();
        assert_eq!(props["text"], "cargo test");
        assert_eq!(props["wrap"], true);
        assert_eq!(props["numbers"], false);
    }
    let failed = find_node(&surface.sent.main, &id(ids[done], "command.label")).unwrap();
    let spans = &failed.p.as_ref().unwrap().as_map()["spans"];
    assert_eq!(spans[1]["t"], " · error");
    assert_eq!(spans[1]["s"], "error");
    assert_eq!(spans[2]["t"], " · exit 1");
    shell.state.borrow_mut().verbose_tools = true;
    ack(&shell, 1);
    surface.flush(&shell.state).unwrap();
    for index in [running, done] {
        assert!(find_node(&surface.sent.main, &id(ids[index], "out")).is_some());
    }
}

#[test]
fn os_focus_return_reasserts_native_focus_without_a_visible_event() {
    let (shell, mut surface, output) = setup(2);
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 1);
    // No model change and no TSP `Visible`: nothing is sent.
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 1);
    // OS focus return (back from screen recording / another app): force a
    // frame and re-assert `composer.editor` focus even though Tern sent no
    // visibility event.
    shell.request_tern_focus_resync();
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.messages("f").len(), 2);
    let frame = output.last_frame();
    assert!(
        frame["ops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op[0] == "focus" && op[1] == "composer.editor"),
        "focus return must re-assert native keyboard focus: {frame:?}"
    );
}

#[test]
fn tern_policy_gates_the_native_backend() {
    use crate::config::TernMode;
    // Off denies negotiation even inside an advertising terminal; On forces it
    // where nothing advertises; Auto defers to the terminal, and a test binary
    // never negotiates implicitly.
    for (mode, term, expected) in [
        (TernMode::Off, Some("tern"), false),
        (TernMode::Off, None, false),
        (TernMode::On, None, true),
        (TernMode::On, Some("iterm"), true),
        (TernMode::Auto, Some("tern"), true),
        (TernMode::Auto, Some("iTerm.app"), false),
        (TernMode::Auto, None, false),
    ] {
        assert_eq!(
            super::decide(mode, term, false),
            expected,
            "{mode:?}/{term:?}"
        );
        // Test binaries keep the explicit opt-in requirement.
        if mode == TernMode::Auto {
            assert!(!super::decide(mode, term, true), "{mode:?}/{term:?}");
        }
    }
}

#[test]
fn the_published_policy_round_trips_through_the_shared_gate() {
    use crate::config::TernMode;
    let before = super::policy();
    for mode in [TernMode::On, TernMode::Off, TernMode::Auto] {
        super::set_policy(mode);
        assert_eq!(super::policy(), mode);
    }
    // The cached answer the input filter reads follows the same gate.
    super::set_policy(TernMode::Off);
    assert!(!super::enabled());
    super::set_policy(before);
}

#[test]
fn markdown_projection_collapses_stacked_blank_lines_but_preserves_fences() {
    assert_eq!(super::tighten_markdown("a\n\n\n\nb"), "a\n\nb");
    assert_eq!(
        super::tighten_markdown("\n\n# Title\n\nbody\n\n"),
        "# Title\n\nbody"
    );
    let fenced = "text\n\n```rust\nline1\n\n\nline2\n```\nafter";
    assert_eq!(
        super::tighten_markdown(fenced),
        "text\n\n```rust\nline1\n\n\nline2\n```\nafter"
    );
}

#[test]
fn user_prompt_is_a_native_card_tern_sizes_at_any_width() {
    for cols in [60, 200] {
        let (mut shell, mut surface, _) = setup(2);
        shell.set_size(cols, 20);
        shell.state.borrow_mut().push_block(TranscriptBlock::User {
            text: "hey what's **octet**?".into(),
            model_lab: Some(ModelLab::Anthropic),
            prompt_color: Some("#d97757".into()),
            persisted: true,
        });
        surface.flush(&shell.state).unwrap();
        let identity = shell.state.borrow().transcript_commit_ids[0];
        let card = find_node(&surface.sent.main, &id(identity, "user")).unwrap();
        // No ANSI rows padded to octet's idea of the width: Tern lays the card
        // out against its own column, so the fill can never come out ragged.
        assert_eq!(card.k, Kind::Card, "{cols}");
        let props = card.p.as_ref().unwrap().as_map();
        assert_eq!(props["tone"], "user");
        assert_eq!(props["role"], "omp.user");
        let body = find_node(&surface.sent.main, &id(identity, "user.body")).unwrap();
        assert_eq!(body.k, Kind::Md, "{cols}");
        let text = body.p.as_ref().unwrap().as_map()["text"].as_str().unwrap();
        assert_eq!(text, "hey what's **octet**?");
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }
}

#[test]
fn ordinary_picker_is_a_compact_sheet_with_filter_full_selection_and_option_count() {
    let (mut shell, mut surface, _) = setup(2);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Select model"),
        items: vec!["claude-opus".into(), "gpt-5".into()],
        descriptions: vec![Some("Full selected model detail λ🙂".into()), None],
        selected: 0,
        filter: "op".into(),
        action: PanelAction::SelectModel(Vec::new()),
    });
    surface.flush(&shell.state).unwrap();
    let panel = super::super::tern_picker::id(&shell.state.borrow());
    assert_eq!(surface.sent.focus.as_deref(), Some(panel.as_str()));
    let sheet = find_node(&surface.sent.layer, &format!("{panel}.sheet")).unwrap();
    assert_eq!(sheet.k, Kind::Overlay);
    let props = sheet.p.as_ref().unwrap().as_map();
    assert_eq!(props["modal"], true);
    assert_eq!(props["size"], "md");
    walk(&surface.sent.layer, &mut |node| {
        assert_ne!(node.k, Kind::Picker)
    });
    let filter = find_node(&surface.sent.layer, &format!("{panel}.filter")).unwrap();
    assert_eq!(filter.k, Kind::Editor);
    assert_eq!(filter.p.as_ref().unwrap().as_map()["text"], "op");
    assert_eq!(filter.p.as_ref().unwrap().as_map()["maxLines"], 1);
    let list = find_node(&surface.sent.layer, &panel).unwrap();
    assert_eq!(list.k, Kind::List);
    assert_eq!(
        list.p.as_ref().unwrap().as_map()["selected"],
        format!("{panel}.item.0")
    );
    assert_eq!(list.c.as_ref().unwrap().len(), 1);
    let item = &list.c.as_ref().unwrap()[0];
    assert_eq!(item.k, Kind::Item);
    assert_eq!(item.p.as_ref().unwrap().as_map()["label"], "claude-opus");
    for (suffix, expected) in [
        ("selected", "claude-opus"),
        ("detail", "Full selected model detail λ🙂"),
        ("count", "1 of 2 options"),
    ] {
        let node = find_node(&surface.sent.layer, &format!("{panel}.{suffix}")).unwrap();
        assert_eq!(node.p.as_ref().unwrap().as_map()["spans"][0]["t"], expected);
        if suffix != "count" {
            assert_eq!(node.p.as_ref().unwrap().as_map()["wrap"], "word");
        }
    }
    assert_eq!(
        find_node(&surface.sent.layer, &format!("{panel}.confirm.key"))
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["keys"],
        json!(["enter"])
    );
}

#[test]
fn provider_scopes_filter_host_indices_and_reject_stale_or_noncanonical_ids() {
    use super::super::tern_picker;
    let (mut shell, _, _) = setup(2);
    shell.state.native().lock().unwrap().accepting_input = true;
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Models"),
        items: vec!["alpha".into(), "beta".into(), "gamma".into()],
        descriptions: vec![None; 3],
        selected: 2,
        filter: String::new(),
        action: PanelAction::SelectGroupedModel {
            models: ["a", "b", "c"]
                .map(|id| octet_ai::ModelId(id.into()))
                .to_vec(),
            providers: vec!["one".into(), "two".into(), "one".into()],
            details: Vec::new(),
            scope: None,
        },
    });
    let handler = shell.tern_input_handler();
    let old_id = tern_picker::id(&shell.state.borrow());
    let scope = |id: &str, value: &str| {
        Incoming::Event(Event::Action {
            sf: SURFACE.into(),
            id: id.into(),
            act: "scope".into(),
            value: Some(value.into()),
            mods: None,
        })
    };
    handler(scope(&old_id, "provider.01"));
    assert_eq!(tern_picker::id(&shell.state.borrow()), old_id);
    handler(scope(&old_id, "provider.2")); // duplicate, not a sidebar id
    assert_eq!(tern_picker::id(&shell.state.borrow()), old_id);
    handler(scope(&old_id, "provider.1"));
    let current = tern_picker::id(&shell.state.borrow());
    assert_ne!(current, old_id);
    let node = tern_picker::node(&shell.state.borrow()).unwrap();
    assert_eq!(
        node.p.as_ref().unwrap().as_map()["order"],
        json!([{"group":"provider.1","label":"two","count":1},"1"])
    );
    handler(scope(&old_id, "all"));
    assert_eq!(tern_picker::id(&shell.state.borrow()), current);
    assert!(tern_picker::select(&mut shell.state.borrow_mut(), "0").is_none());
    assert!(tern_picker::select(&mut shell.state.borrow_mut(), "1").is_some());
    handler(scope(&current, "all"));
    assert_ne!(tern_picker::id(&shell.state.borrow()), current);
}

#[test]
fn thinking_sheet_routes_exact_owned_controls_and_keeps_native_list_focus() {
    use crate::config::ThinkingLevel;
    let (mut shell, mut surface, _) = setup(2);
    shell.state.native().lock().unwrap().accepting_input = true;
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Thinking"),
        items: vec!["off".into(), "high".into()],
        descriptions: vec![None; 2],
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectThinking(vec![ThinkingLevel::Off, ThinkingLevel::High]),
    });
    surface.flush(&shell.state).unwrap();
    let panel = super::super::tern_picker::id(&shell.state.borrow());
    assert_eq!(surface.sent.focus.as_deref(), Some(panel.as_str()));
    assert_eq!(surface.sent.layer[0].k, Kind::Overlay);
    assert!(find_node(&surface.sent.layer, &panel).is_some_and(|node| node.k == Kind::List));
    let handler = shell.tern_input_handler();
    let action = |id: String| {
        Incoming::Event(Event::Action {
            sf: SURFACE.into(),
            id,
            act: "confirm".into(),
            value: None,
            mods: None,
        })
    };
    assert!(handler(action(format!("{panel}.actions"))).is_none());
    assert!(handler(action(format!("{panel}.confirm.suffix"))).is_none());
    assert!(handler(action(format!("{panel}.confirm"))).is_some());
    handler(Incoming::Event(Event::Select {
        sf: SURFACE.into(),
        id: panel.clone(),
        item: format!("{panel}.item.1"),
    }));
    assert!(matches!(
        shell.state.borrow().panel.as_ref(),
        Some(Panel::SelectList { selected: 1, .. })
    ));
    let close = |id: String| {
        Incoming::Event(Event::Action {
            sf: SURFACE.into(),
            id,
            act: "close".into(),
            value: None,
            mods: None,
        })
    };
    assert!(handler(close(format!("{panel}.sheet.suffix"))).is_none());
    assert!(handler(close(format!("{panel}.sheet"))).is_some());
    shell.close_panel();
    assert!(handler(action(format!("{panel}.confirm"))).is_none());
    assert!(handler(close(format!("{panel}.sheet"))).is_none());
}

#[test]
fn welcome_owns_its_layout_and_avoids_omp_logo_overrides() {
    let (shell, mut surface, _) = setup(8);
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    for cols in [20, 46, 120, 240] {
        shell.state.borrow_mut().size = (cols, 40);
        surface.flush(&shell.state).unwrap();
        let welcome = find_node(&surface.sent.main, "welcome").unwrap();
        assert_eq!(welcome.k, Kind::Row);
        let props = welcome.p.as_ref().unwrap().as_map();
        assert_eq!(props["role"], "octet.welcome");
        assert_eq!(props["align"], "center");
        assert_eq!(props["gap"], "sm");
        assert_eq!(props["wrap"], true);
        let children = welcome.c.as_ref().unwrap();
        assert_eq!(children.len(), 6);
        assert!(children.iter().all(|node| node.c.is_none()));
        assert!(find_node(&surface.sent.main, "welcome.grid").is_none());
        assert!(find_node(&surface.sent.main, "welcome.brand").is_none());
        for node in children.iter().filter(|node| node.k == Kind::Text) {
            assert_eq!(node.p.as_ref().unwrap().as_map()["wrap"], "word");
            assert!(!node.p.as_ref().unwrap().as_map()["spans"]
                .as_array()
                .unwrap()
                .iter()
                .any(|span| span["t"].as_str().unwrap().contains('\n')));
        }
        let mark = find_node(&surface.sent.main, "welcome.byte").unwrap();
        let props = mark.p.as_ref().unwrap().as_map();
        assert_eq!(mark.k, Kind::Image);
        assert_eq!(props["role"], "octet.welcome.logo");
        assert_eq!(props["w"], 40);
        assert_eq!(props["h"], 20);
        assert_eq!(props["alt"], "octet byte mark: 01101111");
        assert!(!serde_json::to_string(welcome)
            .unwrap()
            .contains("omp.welcome"));
    }
    let fallback = super::super::tern_welcome::node(&shell.state.borrow(), &Default::default());
    let mark = find_node(std::slice::from_ref(&fallback), "welcome.byte").unwrap();
    assert_eq!(mark.k, Kind::Text);
    assert_eq!(
        mark.p.as_ref().unwrap().as_map()["spans"][0]["t"],
        "01101111"
    );
    assert!(!serde_json::to_string(&fallback)
        .unwrap()
        .contains("omp.welcome"));
}

#[test]
fn welcome_uploads_identical_hashed_bytes_once_and_replays_after_eviction() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use sha2::{Digest, Sha256};
    let (shell, mut surface, output) = setup(8);
    shell.state.borrow_mut().startup_card_started_at = Some(Instant::now());
    surface.flush(&shell.state).unwrap();
    let blobs = output.blobs();
    assert_eq!(blobs.len(), 1);
    let bytes = STANDARD.decode(&blobs[0].body).unwrap();
    assert_eq!(
        blobs[0].params["id"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(blobs[0].params["mime"], "image/png");
    assert!(bytes.len() < 4096);
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    // Exercise the existing independent image decoder through its public media boundary.
    let image = octet_ai::types::ImageMedia {
        source: octet_ai::types::ImageSource::Inline(bytes::Bytes::from(bytes.clone())),
        media_type: Some(mime::IMAGE_PNG),
        detail: None,
    };
    octet_ai::media::prepare_user_image(
        &image,
        octet_ai::media::ImageInputLimits {
            max_width: 256,
            max_height: 128,
            max_bytes: 4096,
        },
    )
    .unwrap();
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 256);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 128);
    let mut compressed = Vec::new();
    let mut offset = 8;
    while offset < bytes.len() {
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        if &bytes[offset + 4..offset + 8] == b"IDAT" {
            compressed.extend_from_slice(&bytes[offset + 8..offset + 8 + len]);
        }
        offset += len + 12;
    }
    let mut pixels = Vec::new();
    io::Read::read_to_end(
        &mut flate2::read::ZlibDecoder::new(compressed.as_slice()),
        &mut pixels,
    )
    .unwrap();
    assert_eq!(pixels.len(), 128 * (1 + 256 * 4));
    for y in 0..128 {
        assert_eq!(pixels[y * (1 + 256 * 4)], 0);
        for x in 0..256 {
            let alpha = pixels[y * (1 + 256 * 4) + 1 + x * 4 + 3];
            let expected = if b"01101111"[x / 32] == b'0' && y < 64 {
                0
            } else {
                255
            };
            assert_eq!(alpha, expected, "eight-bar silhouette at ({x}, {y})");
        }
    }
    let mark = find_node(&surface.sent.main, "welcome.byte").unwrap();
    let props = mark.p.as_ref().unwrap().as_map();
    assert_eq!(props["w"], 40);
    assert_eq!(props["h"], 20);
    assert_eq!(props["blob"], blobs[0].params["id"]);
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.blobs().len(), 1);
    surface
        .observe(&Incoming::Event(Event::Gone {
            sf: Some(SURFACE.into()),
            ids: vec!["main".into()],
        }))
        .unwrap();
    surface.flush(&shell.state).unwrap();
    assert_eq!(output.blobs().len(), 2);
    assert_eq!(
        output.blobs()[0].params["id"],
        output.blobs()[1].params["id"]
    );
    assert_eq!(
        output.blobs()[0],
        output.blobs()[1],
        "eviction replays the exact addressed PNG bytes"
    );
}

#[test]
fn native_transcript_and_composer_share_the_rail_responsive_measure() {
    let (shell, mut surface, _) = setup(8);
    {
        let mut state = shell.state.borrow_mut();
        state.startup_card_started_at = Some(Instant::now());
        state.push_block(TranscriptBlock::Notice("Layout fixture".into()));
    }
    for cols in [46, 120, 240] {
        shell.state.borrow_mut().size = (cols, 40);
        surface.flush(&shell.state).unwrap();
        for node in &surface.sent.main {
            assert_eq!(node.p.as_ref().unwrap().as_map()["max"]["w"], "96ch");
            assert!(node.p.as_ref().unwrap().as_map().get("min").is_none());
        }
        let dock = find_node(&surface.sent.dock, "dock.content").unwrap();
        let composer = find_node(dock.c.as_deref().unwrap(), "composer").unwrap();
        assert_eq!(composer.p.as_ref().unwrap().as_map()["max"]["w"], "96ch");
    }
}

#[test]
fn model_preview_carries_only_bounded_public_facts() {
    let (mut shell, mut surface, _) = setup(2);
    let detail = crate::tui::pickers::ModelPickerDetail {
        name: "Model".into(),
        context: 100_000,
        output: 16_000,
        price: "$1 · $2".into(),
        cache_price: Some("$0.1".into()),
        input: "text · image".into(),
        badges: vec!["vision".into(), "tools".into()],
        source: vec![
            ("knowledge".into(), "2026-01".into()),
            ("api_key".into(), "SECRET".into()),
        ],
    };
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Models"),
        items: vec!["model".into()],
        descriptions: vec![None],
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectGroupedModel {
            models: vec![octet_ai::ModelId("model".into())],
            providers: vec!["provider".into()],
            details: vec![detail],
            scope: None,
        },
    });
    surface.flush(&shell.state).unwrap();
    assert_eq!(surface.sent.layer[0].k, Kind::Picker);
    let json = serde_json::to_string(&surface.sent.layer).unwrap();
    assert!(json.contains("Cache read / M"));
    assert!(json.contains("2026-01"));
    assert!(json.contains("vision"));
    assert!(!json.contains("SECRET"));
    assert!(!json.contains("api_key"));
}

#[test]
fn session_picker_keeps_preview_actions_loading_and_host_selection() {
    use super::super::{OrdinarySurfaceLifecycle, PickerState};
    use crate::session_store::SessionMeta;
    let (mut shell, mut surface, _) = setup(2);
    let path = std::path::PathBuf::from("/tmp/local-session.jsonl");
    let session = SessionMeta {
        id: "session".into(),
        path: path.clone(),
        title: "First prompt".into(),
        name: Some("Named session".into()),
        tags: vec!["work".into()],
        pinned: false,
        archived: false,
        trashed_at_ms: None,
        purge_after_ms: None,
        forked_from_session_id: Some("parent".into()),
        forked_from_entry_id: None,
        message_count: 12,
        modified: std::time::SystemTime::now(),
        workspace: Some("/tmp/project".into()),
    };
    let mut picker = PickerState::new(vec![session], Some(path));
    picker.surface.lifecycle = OrdinarySurfaceLifecycle::loading("all workspaces");
    shell.open_panel(Panel::SessionPicker { picker });
    surface.flush(&shell.state).unwrap();
    assert_eq!(surface.sent.layer[0].k, Kind::Picker);
    let props = surface.sent.layer[0].p.as_ref().unwrap().as_map();
    assert_eq!(props["noun"], "sessions");
    assert_eq!(props["state"], "loading");
    assert_eq!(props["message"], "all workspaces");
    assert_eq!(props["current"], json!(["0"]));
    assert!(props["actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["id"] == "delete" && action["disabled"] == true));
    assert!(find_node(&surface.sent.layer, "panel.session.facts").is_some());
    shell.state.native().lock().unwrap().accepting_input = true;
    let handler = shell.tern_input_handler();
    let id = super::super::tern_picker::id(&shell.state.borrow());
    let action = |act: &str| {
        Incoming::Event(Event::Action {
            sf: SURFACE.into(),
            id: id.clone(),
            act: act.into(),
            value: None,
            mods: None,
        })
    };
    assert_eq!(
        handler(action("rename")),
        Some(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('r'),
                crossterm::event::KeyModifiers::CONTROL
            )
        ))
    );
    assert_eq!(
        handler(action("sort")),
        Some(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('s'),
                crossterm::event::KeyModifiers::CONTROL
            )
        ))
    );
    assert!(handler(Incoming::Event(Event::Action {
        sf: SURFACE.into(),
        id: id.clone(),
        act: "strip".into(),
        value: Some("named".into()),
        mods: None
    }))
    .is_some());
    assert!(handler(Incoming::Event(Event::Action {
        sf: SURFACE.into(),
        id: id.clone(),
        act: "strip".into(),
        value: Some("delete".into()),
        mods: None
    }))
    .is_none());
    shell.set_size(60, 30);
    let node = super::super::tern_picker::node(&shell.state.borrow()).unwrap();
    assert_eq!(node.p.as_ref().unwrap().as_map()["preview"], "below");
    assert_eq!(
        node.p.as_ref().unwrap().as_map()["strip"]["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    shell.close_panel();
    assert!(handler(action("rename")).is_none());
}

#[test]
fn native_agents_keep_one_retained_transcript_with_authoritative_live_token_lines() {
    use super::super::SubagentActivityView;
    let (shell, mut surface, output) = setup(8);
    let mut child = octet_agent::DelegationTelemetryChild {
        child_id: "child".into(),
        task_name: "Inspect".into(),
        profile: Some("explore".into()),
        model: "model".into(),
        state: "running".into(),
        phase: "using_tool".into(),
        current_tool: Some("read".into()),
        tool_use_count: 3,
        input_tokens: 100,
        cache_read_tokens: 20,
        cache_write_tokens: 10,
        output_tokens: 50,
        estimated_output_tokens: Some(60),
        reasoning_tokens: 30,
        total_tokens: 180,
        cost: None,
        cost_microdollars: Some(2_500),
        elapsed_ms: 1200,
        failure_class: None,
        failure_reason: None,
        effective_tool_policy: octet_agent::SandboxConfig::new(".")
            .effective_tool_policy(octet_agent::EffectPolicy::Controlled),
        orchestration_provenance: octet_agent::DelegationOrchestrationProvenance::all(
            octet_agent::DelegationPolicySource::ParentInherited,
        ),
        session: None,
    };
    let snapshot = |child| SubagentActivityView {
        status_label: "1 running".into(),
        telemetry: vec![child],
        ..Default::default()
    };
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(snapshot(child.clone()));
    assert_eq!(shell.state.borrow().transcript.len(), 1);
    let identity = shell.state.borrow().transcript_commit_ids[0];
    let root_id = id(identity, "subagents");
    let tokens_id = id(identity, "subagents.worker0.tokens");
    surface.flush(&shell.state).unwrap();
    let dock = find_node(&surface.sent.dock, "dock.content").unwrap();
    assert!(dock.p.as_ref().unwrap().as_map().get("role").is_none());
    for node in dock.c.as_ref().unwrap() {
        assert_eq!(node.p.as_ref().unwrap().as_map()["max"]["w"], "96ch");
    }
    walk(&surface.sent.dock, &mut |node| {
        assert_ne!(node.k, Kind::Agent);
        assert!(!node.id.contains("subagent"), "no duplicate dock roster");
    });
    let transcript = find_node(&surface.sent.main, &root_id).unwrap();
    assert_eq!(transcript.k, Kind::Row);
    assert_eq!(
        transcript.p.as_ref().unwrap().as_map()["role"],
        "octet.subagents"
    );
    let tokens = find_node(&surface.sent.main, &tokens_id).unwrap();
    // Input includes the three authoritative categories, not total/reasoning;
    // streamed output is explicitly estimated instead of double-counted.
    assert_eq!(
        tokens.p.as_ref().unwrap().as_map()["spans"][0]["t"],
        "· ↑130 ↓~60"
    );
    assert_eq!(
        find_node(&surface.sent.main, &id(identity, "subagents.worker0.name"))
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["spans"][0]["t"],
        "Inspect"
    );
    assert!(find_node(&surface.sent.main, &id(identity, "subagents.stop")).is_some());
    walk(std::slice::from_ref(transcript), &mut |node| {
        assert!(matches!(node.k, Kind::Row | Kind::Col | Kind::Text));
        let props = node.p.as_ref().unwrap().as_map();
        for forbidden in ["model", "cost", "stats", "prompt", "tool", "age", "took"] {
            assert!(
                !props.contains_key(forbidden),
                "private inspector fact: {forbidden}"
            );
        }
    });
    let serialized = serde_json::to_string(transcript).unwrap();
    for private in ["using_tool", "explore", "read", "0.0025"] {
        assert!(
            !serialized.contains(private),
            "private inspector fact: {private}"
        );
    }
    let mut retained_ids = Vec::new();
    walk(std::slice::from_ref(transcript), &mut |node| {
        retained_ids.push(node.id.clone())
    });
    child.estimated_output_tokens = Some(65);
    child.cost_microdollars = None;
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(snapshot(child.clone()));
    surface.flush(&shell.state).unwrap();
    assert_eq!(shell.state.borrow().transcript.len(), 1);
    assert_eq!(shell.state.borrow().transcript_commit_ids[0], identity);
    let transcript = find_node(&surface.sent.main, &root_id).unwrap();
    let mut updated_ids = Vec::new();
    walk(std::slice::from_ref(transcript), &mut |node| {
        updated_ids.push(node.id.clone())
    });
    assert_eq!(updated_ids, retained_ids);
    assert_eq!(
        find_node(&surface.sent.main, &tokens_id)
            .unwrap()
            .p
            .as_ref()
            .unwrap()
            .as_map()["spans"][0]["t"],
        "· ↑130 ↓~65"
    );
    let frame = output.last_frame();
    let ops = frame["ops"].as_array().unwrap();
    assert!(ops.iter().any(|op| op[0] == "set" && op[1] == tokens_id));
    assert!(!ops
        .iter()
        .any(|op| matches!(op[0].as_str(), Some("add" | "del" | "move"))));
    // Settlement preserves the orchestration identity but removes live detail.
    child.state = "completed".into();
    child.estimated_output_tokens = None;
    shell
        .state
        .borrow_mut()
        .set_subagent_activity(snapshot(child));
    surface.flush(&shell.state).unwrap();
    assert_eq!(shell.state.borrow().transcript.len(), 1);
    assert_eq!(shell.state.borrow().transcript_commit_ids[0], identity);
    let settled = find_node(&surface.sent.main, &root_id).unwrap();
    walk(std::slice::from_ref(settled), &mut |node| {
        assert!(!node.id.contains(".worker"));
        assert!(!node.id.ends_with(".stop"));
    });
    assert!(serde_json::to_string(settled)
        .unwrap()
        .contains("1 completed"));
    let marker = find_node(&surface.sent.main, &id(identity, "subagents.marker")).unwrap();
    assert_eq!(
        marker.p.as_ref().unwrap().as_map()["spans"][0]["s"],
        "success"
    );
    assert!(marker.p.as_ref().unwrap().as_map()["spans"][0]
        .get("fx")
        .is_none());
    assert!(!output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| matches!(op[0].as_str(), Some("add" | "del")) && op[1] == root_id));
}

#[path = "tern_rail_tests.rs"]
mod rail;

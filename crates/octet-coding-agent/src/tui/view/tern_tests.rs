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
    let client = TernClient::with_writer("test", None, output.clone()).unwrap();
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
fn composer_and_completions_are_flat_native_octet_not_omp_glass_or_rows() {
    let (shell, mut surface, _) = setup(2);
    shell.state.borrow_mut().editor.set_text("/");
    shell.state.borrow_mut().context_estimate = Some((1234, 131072));
    surface.flush(&shell.state).unwrap();
    let projection = &surface.sent;
    let composer = find_node(&projection.dock, "composer").unwrap();
    assert_eq!(
        composer.p.as_ref().unwrap().as_map()["role"],
        "octet.composer"
    );
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
        find_node(&surface.sent.main, &id(identity, "assistant"))
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
            && op[1] == id(identity, "assistant")
            && op[2]["text"] == "hello 🦀 world"));
    assert!(!frame["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| op[0] == "add" || op[0] == "del"));
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
    assert!(!output.last_frame()["ops"]
        .as_array()
        .unwrap()
        .iter()
        .any(|op| (op[0] == "add" || op[0] == "del") && op[1] == user_id));
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

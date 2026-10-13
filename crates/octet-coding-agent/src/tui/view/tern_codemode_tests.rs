//! Synthetic TSP regressions for Codemode and native queued-input chrome.
use super::*;
use crate::presentation::summarize_tool;
use crate::tui::view::{ComposedInput, ToolPanel};
use octet_ai::ToolCallId;

#[test]
fn codemode_native_source_and_output_preview_expand_without_replacing_nodes() {
    for (finished, failed) in [(false, false), (true, false), (true, true)] {
        let (shell, mut surface, output) = setup(10);
        let code = "const x = '雪🦀';\n\nreturn x;";
        let result = format!(
            "Script completed\nWall time 0.1 seconds\nOutput:\n{}",
            "JSON output ".repeat(200)
        );
        let args = json!({"code": code});
        let index = shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId("codemode-native".into()),
                "codemode".into(),
                args.to_string(),
                summarize_tool("codemode", &args),
                result.clone(),
                finished,
                failed,
                failed.then(|| "SyntaxError: unexpected token".into()),
                None,
            ))));
        let identity = shell.state.borrow().transcript_commit_ids[index];
        surface.flush(&shell.state).unwrap();
        let source_id = id(identity, "command");
        let output_id = id(identity, "out");
        let source = find_node(&surface.sent.main, &source_id).unwrap();
        assert_eq!(source.k, Kind::Code);
        let props = source.p.as_ref().unwrap().as_map();
        assert_eq!(props["text"], code);
        assert_eq!(props["lang"], "javascript");
        assert_eq!(props["wrap"], true);
        let shown = find_node(&surface.sent.main, &output_id).unwrap();
        assert_eq!(shown.k, Kind::Code);
        let preview = shown.p.as_ref().unwrap().as_map()["text"].as_str().unwrap();
        assert!(preview.contains("Script completed"));
        assert!(preview.contains("Ctrl+O for full output"));
        assert!(preview.chars().count() < 700);
        let root = find_node(&surface.sent.main, &id(identity, "tool")).unwrap();
        let root_props = root.p.as_ref().unwrap().as_map();
        assert_eq!(root.k, Kind::Tool);
        assert_eq!(root_props["role"], "omp.tool.codemode");
        assert_eq!(root_props["collapsed"], false);
        assert_eq!(root_props["collapsible"], false);
        shell.state.borrow_mut().verbose_tools = true;
        surface.flush(&shell.state).unwrap();
        let full = find_node(&surface.sent.main, &output_id).unwrap();
        assert_eq!(full.p.as_ref().unwrap().as_map()["text"], result);
        assert!(!output.last_frame()["ops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| matches!(op[0].as_str(), Some("del" | "add")) && op[1] == output_id));
        shell.state.borrow_mut().verbose_tools = false;
        surface.flush(&shell.state).unwrap();
        let hidden = find_node(&surface.sent.main, &output_id).unwrap();
        assert!(hidden.p.as_ref().unwrap().as_map()["text"]
            .as_str()
            .unwrap()
            .contains("output preview"));
        let state = shell.state.borrow();
        let TranscriptBlock::Tool(panel) = &state.transcript[index] else {
            panic!("tool")
        };
        assert_eq!(panel.output, result);
        assert_eq!(panel.args, args.to_string());
    }
}

#[test]
fn native_tools_share_one_header_and_read_has_only_one_path() {
    let (shell, mut surface, _) = setup(3);
    shell.state.borrow_mut().workspace = Some("/tmp/native-fixture".into());
    for (name, args) in [
        ("read", json!({"path":"src/main.rs"})),
        ("bash", json!({"command":"cargo test"})),
        ("codemode", json!({"code":"return 1;"})),
    ] {
        let mut panel = ToolPanel::new(
            ToolCallId(name.into()),
            name.into(),
            args.to_string(),
            summarize_tool(name, &args),
            "captured output".into(),
            true,
            false,
            None,
            None,
        );
        panel.duration = Some(Duration::from_millis(42));
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Tool(Box::new(panel)));
    }
    surface.flush(&shell.state).unwrap();
    let identities = shell.state.borrow().transcript_commit_ids.clone();
    for (identity, name) in identities.into_iter().zip(["read", "bash", "codemode"]) {
        let root = find_node(&surface.sent.main, &id(identity, "tool")).unwrap();
        assert_eq!(root.k, Kind::Tool, "shared native glyph/header: {name}");
        let props = root.p.as_ref().unwrap().as_map();
        assert_eq!(props["name"], name);
        assert_eq!(props["frame"], "inline");
        assert_eq!(props["status"], "done");
        assert_eq!(props["meta"], json!(["42ms"]));
        assert_eq!(props["collapsible"], false);
        if name == "read" {
            assert_eq!(props["target"], "src/main.rs");
            assert!(props["href"].as_str().unwrap().ends_with("/src/main.rs"));
            assert!(
                root.c.as_deref().unwrap_or_default().is_empty(),
                "no second file row"
            );
        } else {
            assert_eq!(props["target"], "");
            assert_eq!(
                find_node(&surface.sent.main, &id(identity, "command"))
                    .unwrap()
                    .k,
                Kind::Code
            );
        }
    }
}

#[test]
fn codemode_output_is_literal_even_when_it_looks_like_diff_or_markdown() {
    let (shell, mut surface, _) = setup(2);
    let args = json!({"code":"return 1"});
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("codemode-literal".into()),
            "codemode".into(),
            args.to_string(),
            summarize_tool("codemode", &args),
            "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-# old\n+# new".into(),
            true,
            false,
            None,
            None,
        ))));
    surface.flush(&shell.state).unwrap();
    walk(&surface.sent.main, &mut |node| {
        assert!(!matches!(node.k, Kind::Diff | Kind::Md))
    });
}

#[test]
fn queued_input_native_chrome_is_bounded_readonly_and_tracks_queue_ownership() {
    for width in [20, 46, 120] {
        let (mut shell, mut surface, _) = setup(10);
        shell.state.borrow_mut().size = (width, 24);
        shell.prefill_editor("parent draft".into());
        shell.queue_steering(&ComposedInput::from_text("oldest\n\x1b[3J hostile".into()));
        surface.flush(&shell.state).unwrap();
        let pending = find_node(&surface.sent.dock, "pending").unwrap();
        assert_eq!(pending.k, Kind::Card);
        assert!(
            find_node(&surface.sent.dock, "pending.key").is_none(),
            "nonretractable steering has no edit hint"
        );
        assert_eq!(
            find_node(&surface.sent.dock, "pending.state").unwrap().k,
            Kind::Badge
        );
        shell.queue_follow_up(ComposedInput::from_text("newer follow-up".into()));
        surface.flush(&shell.state).unwrap();
        assert!(find_node(&surface.sent.dock, "pending.key").is_some());
        assert_eq!(
            find_node(&surface.sent.dock, "pending.state")
                .unwrap()
                .p
                .as_ref()
                .unwrap()
                .as_map()["text"],
            "2 queued"
        );
        let preview = find_node(&surface.sent.dock, "pending.preview").unwrap();
        assert!(serde_json::to_string(preview).unwrap().contains("oldest"));
        assert!(!serde_json::to_string(preview).unwrap().contains("\\u001b"));
        walk(
            std::slice::from_ref(find_node(&surface.sent.dock, "pending").unwrap()),
            &mut |node| {
                assert_ne!(node.k, Kind::Ansi);
                assert!(node
                    .p
                    .as_ref()
                    .is_none_or(|p| !p.as_map().contains_key("actions")));
            },
        );
        assert_eq!(shell.pending(), "parent draft");
        shell.state.borrow_mut().steering_queue = Default::default();
        shell.state.borrow_mut().follow_up_queue = Default::default();
        surface.flush(&shell.state).unwrap();
        assert!(find_node(&surface.sent.dock, "pending").is_none());
    }
}

#[test]
fn queued_input_native_key_hint_respects_remapped_and_disabled_bindings() {
    use crate::tui::keymap::keybindings::KeybindingsManager;
    use std::collections::BTreeMap;
    for keys in [vec!["ctrl+y".into()], Vec::new()] {
        let (mut shell, mut surface, _) = setup(2);
        shell.test_set_keybindings(KeybindingsManager::with_platform(
            "linux",
            false,
            BTreeMap::from([("app.message.dequeue".into(), keys.clone())]),
        ));
        let _handler = shell.tern_input_handler();
        shell.queue_follow_up(ComposedInput::from_text("queued".into()));
        surface.flush(&shell.state).unwrap();
        let key = find_node(&surface.sent.dock, "pending.key");
        if keys.is_empty() {
            assert!(key.is_none());
        } else {
            assert_eq!(
                key.unwrap().p.as_ref().unwrap().as_map()["keys"],
                json!(["ctrl+y"])
            );
        }
    }
}

#[test]
fn safe_boundary_controls_share_native_queue_chrome_without_input_authority() {
    let (mut shell, mut surface, _) = setup(8);
    shell.set_pending_controls(vec![
        "model local/next · idle".into(),
        "thinking high · next response".into(),
    ]);
    surface.flush(&shell.state).unwrap();
    let preview =
        serde_json::to_string(find_node(&surface.sent.dock, "pending.preview").unwrap()).unwrap();
    assert!(preview.contains("model local/next"));
    assert!(preview.contains("thinking high"));
    assert!(
        find_node(&surface.sent.dock, "pending.key").is_none(),
        "controls are not recallable input"
    );
    assert!(shell.debug_pending_controls().contains("model local/next"));
    shell.set_pending_controls(Vec::new());
    surface.flush(&shell.state).unwrap();
    assert!(find_node(&surface.sent.dock, "pending").is_none());
}

//! Native tree contracts, not desktop or pixel qualification.
use super::*;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crate::tui::theme::{compiled_theme_for_selector, test_theme_from_source, TerminalBackground};

fn themes() -> Vec<crate::tui::theme::OctetTheme> {
    vec![
        compiled_theme_for_selector(
            "pi",
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            TerminalBackground::Dark,
        )
        .unwrap()
        .unwrap(),
        test_theme_from_source(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../extensions/octet-pi-compat/test/fixtures/pi-native-layout.toml"
        ))),
    ]
}

#[test]
fn native_pi_cards_and_editor_rules_preserve_input_and_disclosure_ownership() {
    for theme in themes() {
        let mut state = ShellState {
            theme,
            startup_pending: false,
            context_estimate: Some((100, 1000)),
            ..Default::default()
        };
        state.editor.set_text("雪 draft");
        let node = composer(&state);
        let children = node.c.as_ref().unwrap();
        let ids: Vec<_> = children.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "composer.context",
                "composer.rule",
                "composer.line",
                "composer.rule.bottom",
                "composer.bar"
            ]
        );
        let editor = find_node(children, "composer.editor").unwrap();
        assert_eq!(editor.k, Kind::Editor);
        assert_eq!(
            editor.p.as_ref().unwrap().as_map()["text"],
            json!("雪 draft")
        );
        assert_eq!(focus_target(&state).as_deref(), Some("composer.editor"));
        for name in ["read", "bash", "codemode"] {
            let args = match name {
                "read" => json!({"path":"notes.txt"}),
                "bash" => json!({"command":"printf result"}),
                _ => json!({"code":"return 1"}),
            };
            let block = TranscriptBlock::Tool(Box::new(super::super::ToolPanel::new(
                octet_ai::ToolCallId(name.into()),
                name.into(),
                args.to_string(),
                crate::presentation::summarize_tool(name, &args),
                "fixture output".into(),
                true,
                false,
                None,
                None,
            )));
            let node = block_node(
                1,
                &block,
                &state,
                &super::super::tern_images::NativeImages::default(),
                &HashMap::new(),
                None,
            )
            .unwrap();
            assert_eq!(node.p.as_ref().unwrap().as_map()["frame"], json!("card"));
            if name == "bash" {
                assert_eq!(
                    node.p.as_ref().unwrap().as_map()["collapsible"],
                    json!(false)
                );
                assert!(find_node(node.c.as_deref().unwrap(), "t1.command").is_some());
                assert!(find_node(node.c.as_deref().unwrap(), "t1.out").is_none());
            }
        }
        let assistant = TranscriptBlock::Assistant(Box::new(
            super::super::AssistantBlock::finalized("Unboxed reply".into()),
        ));
        let node = block_node(
            2,
            &assistant,
            &state,
            &super::super::tern_images::NativeImages::default(),
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(node.k, Kind::Col);
        assert_eq!(node.c.as_ref().unwrap()[0].k, Kind::Col);
        assert_eq!(
            find_node(node.c.as_deref().unwrap(), "t2.assistant.md")
                .unwrap()
                .k,
            Kind::Md
        );
    }
}

#[test]
fn native_pi_spinner_changes_glyph_not_label_style_and_honors_reduced_motion() {
    for theme in themes() {
        let mut shell = super::super::InteractiveShell::test_shell_with_theme(theme);
        let run_id = shell.begin_run("test");
        let mut state = shell.state.borrow_mut();
        let before = working_row(&state, false).unwrap();
        state.advance_thinking_spinner(1);
        let after = working_row(&state, false).unwrap();
        assert_eq!(before.c.as_ref().unwrap()[1].k, Kind::Text);
        assert_eq!(before.c.as_ref().unwrap()[1], after.c.as_ref().unwrap()[1]);
        assert_ne!(before.c.as_ref().unwrap()[0], after.c.as_ref().unwrap()[0]);
        assert!(serde_json::to_string(&before).unwrap().contains('⠋'));
        assert!(serde_json::to_string(&after).unwrap().contains('⠙'));
        let reduced = working_row(&state, true).unwrap();
        assert!(!serde_json::to_string(&reduced).unwrap().contains('⠙'));
        drop(state);
        shell.fail_run(run_id, "fixture ended");
        assert!(working_row(&shell.state.borrow(), false).is_none());
    }
}

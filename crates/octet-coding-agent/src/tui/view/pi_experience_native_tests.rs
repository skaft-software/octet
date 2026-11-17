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
        // Pi's flat, rule-framed input is not Tern's editor card, so it keeps its
        // own meter styling rather than the card's top-edge meter.
        let props = node.p.as_ref().unwrap().as_map();
        assert!(props.get("role").is_none());
        assert_eq!(props["gap"], "md");
        assert_eq!(
            find_node(children, "composer.context")
                .unwrap()
                .p
                .as_ref()
                .unwrap()
                .as_map()["role"],
            "octet.composer.context"
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
fn native_extension_working_frames_cannot_override_reduced_motion() {
    for theme in themes() {
        let mut shell = super::super::InteractiveShell::test_shell_with_theme(theme);
        shell.begin_run("test");
        shell.set_extension_ui(super::super::ShellExtensionUi {
            working: Some(super::super::ShellExtensionWorking {
                frames: Some(vec!["custom spinner".into(), "next spinner".into()]),
                interval_ms: Some(60_000),
                ..Default::default()
            }),
            ..Default::default()
        });
        let state = shell.state.borrow();
        let animated = working_row(&state, false).unwrap();
        assert_eq!(animated.c.as_ref().unwrap()[0].k, Kind::Ansi);
        assert!(serde_json::to_string(&animated)
            .unwrap()
            .contains("custom spinner"));
        let reduced = working_row(&state, true).unwrap();
        assert_eq!(reduced.c.as_ref().unwrap()[0].k, Kind::Text);
        assert!(!serde_json::to_string(&reduced)
            .unwrap()
            .contains("custom spinner"));
        drop(state);

        let mut capabilities = shell.theme().capabilities();
        capabilities.animation = false;
        shell.set_theme(crate::tui::theme::test_theme_source_with(
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../extensions/octet-pi-compat/test/fixtures/pi-native-layout.toml"
            )),
            capabilities,
            TerminalBackground::Dark,
        ));
        let static_row = working_row(&shell.state.borrow(), false).unwrap();
        assert_eq!(static_row.c.as_ref().unwrap()[0].k, Kind::Text);
        assert!(!serde_json::to_string(&static_row)
            .unwrap()
            .contains("custom spinner"));
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

#[test]
fn native_working_component_uses_the_spinner_slot_without_added_padding() {
    for theme in themes() {
        let mut shell = super::super::InteractiveShell::test_shell_with_theme(theme);
        shell.begin_run("test");
        for width in [1, 2, 3, 4, 5, 8, 20, 80, 160] {
            shell.set_size(width, 24);
            shell.set_extension_ui(Default::default());
            let native = working_row(&shell.state.borrow(), false).unwrap();
            for line in ["◟ msg", "\x1b[31m界\x1b[0m 雪e\u{301} msg", ">>>  msg"] {
                shell.set_extension_ui(super::super::ShellExtensionUi {
                    working: Some(super::super::ShellExtensionWorking {
                        component_line: Some(line.into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                let component = working_row(&shell.state.borrow(), false).unwrap();
                assert_eq!(
                    component.p, native.p,
                    "native row positioning stays unchanged"
                );
                let children = component.c.as_ref().unwrap();
                assert_eq!(children[0].id, native.c.as_ref().unwrap()[0].id);
                assert_eq!(children[0].k, Kind::Ansi);
                assert_eq!(children[0].p.as_ref().unwrap().as_map()["text"], line);
                assert!(!children.iter().any(|child| child.id == "work.label"));
            }
        }
    }
}

#[test]
fn native_working_component_paints_one_effective_row_without_reanimation() {
    for theme in themes() {
        let mut shell = super::super::InteractiveShell::test_shell_with_theme(theme);
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Reasoning(Box::new(
                super::super::AssistantBlock::finalized_reasoning("settled history".into()),
            )));
        shell.begin_run("test");
        let mut working = super::super::ShellExtensionWorking {
            component_line: Some("\u{1b}[31m>>>\u{1b}[0m patched message".into()),
            message: Some("patched message".into()),
            // Configuration must not override the effective component output.
            frames: Some(vec!["unused".into(), "also unused".into()]),
            interval_ms: Some(1),
            ..Default::default()
        };
        shell.set_extension_ui(super::super::ShellExtensionUi {
            working: Some(working.clone()),
            ..Default::default()
        });
        {
            let mut state = shell.state.borrow_mut();
            let before = working_row(&state, false).unwrap();
            let children = before.c.as_ref().unwrap();
            assert_eq!(children[0].k, Kind::Ansi);
            assert!(!children.iter().any(|child| child.id == "work.label"));
            let serialized = serde_json::to_string(&before).unwrap();
            assert_eq!(serialized.matches("patched message").count(), 1);
            assert!(serialized.contains(">>>"));
            assert!(!serialized.contains("unused"));
            assert!(!state.has_active_thinking_spinner());
            assert!(!state.has_active_status_shimmer());
            let TranscriptBlock::Reasoning(settled) = &state.transcript[0] else {
                panic!("settled reasoning fixture");
            };
            assert!(
                settled.extension_working.is_none(),
                "animation leaves settled history untouched"
            );
            state.advance_thinking_spinner(3);
            state.advance_status_shimmer_by(3);
            let after = working_row(&state, false).unwrap();
            assert_eq!(before.c.as_ref().unwrap()[0], after.c.as_ref().unwrap()[0]);
            let reduced = serde_json::to_string(&working_row(&state, true).unwrap()).unwrap();
            assert!(!reduced.contains(">>>"));
            assert_eq!(reduced.matches("patched message").count(), 1);
        }
        working.component_line = Some("\u{1b}[32m∙∙●∙∙\u{1b}[0m changed".into());
        shell.set_extension_ui(super::super::ShellExtensionUi {
            working: Some(working),
            ..Default::default()
        });
        let next =
            serde_json::to_string(&working_row(&shell.state.borrow(), false).unwrap()).unwrap();
        assert!(next.contains("∙∙●∙∙"));
        assert!(!next.contains(">>>"));
        shell.set_extension_ui(Default::default());
        let fallback =
            serde_json::to_string(&working_row(&shell.state.borrow(), false).unwrap()).unwrap();
        assert!(!fallback.contains("changed"));
        assert!(fallback.contains("work.label"));
    }
}

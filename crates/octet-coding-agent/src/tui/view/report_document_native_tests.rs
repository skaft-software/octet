//! Native report hierarchy uses Markdown nodes, not an ANSI/text dump.
use super::super::InteractiveShell;
use super::*;

#[test]
fn native_fact_reports_use_rich_nodes_without_acquiring_input_or_approval_authority() {
    for (title, text, expected) in [
        ("Status", "Provider       local\n\nSecurity model: local agent\nOS isolation: none\nTelemetry\nTotal tokens   123", "## Safety and permissions"),
        ("Settings", "octet settings\nConfigured model   fixture\nActive reasoning   high\nTheme              pi", "## Launch and session"),
        ("Session", "File: /synthetic/雪_[draft].jsonl\nCost: $0 (known subtotal)", "- **File:**"),
    ] {
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("unsent 雪 draft".into());
        shell.show_report_facts(title, "Read-only facts", text.into());
        let node = report(&shell.state.borrow(), false).unwrap();
        let body = node.c.as_ref().unwrap().iter().find(|node| node.id == "report.body").unwrap();
        assert_eq!(body.k, Kind::Md);
        let source = body.p.as_ref().unwrap().as_map()["text"].as_str().unwrap();
        assert!(source.contains(expected), "{source}");
        assert!(!source.contains('\x1b'));
        assert_eq!(node.p.as_ref().unwrap().as_map()["modal"], json!(true));
        shell.close_overlay();
        assert_eq!(shell.pending(), "unsent 雪 draft");
        assert!(shell.state.borrow().transcript.is_empty());
    }
}

#[test]
fn native_action_menus_separate_strong_labels_from_full_selected_descriptions() {
    use super::super::{OrdinarySurfaceMetadata, Panel, PanelAction, PanelResult};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    for action in [
        PanelAction::SelectSettings(vec![
            crate::commands::SettingsCommand::Theme(None),
            crate::commands::SettingsCommand::Show,
        ]),
        PanelAction::SelectExtension(vec!["first".into(), "second".into()]),
        PanelAction::ProviderSetup(vec!["first".into(), "second".into()]),
    ] {
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("unsent 雪 draft".into());
        shell.open_panel(Panel::SelectList {
            surface: OrdinarySurfaceMetadata::with_purpose("Menu", "Choose an action"),
            items: vec!["First action".into(), "Second action".into()],
            descriptions: vec![
                Some("First description".into()),
                Some("Full selected description 雪".into()),
            ],
            selected: 1,
            filter: String::new(),
            action,
        });
        let state = shell.state.borrow();
        let panel = super::super::tern_picker::id(&state);
        let node = super::super::tern_picker::node(&state).unwrap();
        let nodes = std::slice::from_ref(&node);
        let list = find_node(nodes, &panel).unwrap();
        assert_eq!(list.k, Kind::List);
        assert_eq!(
            list.p.as_ref().unwrap().as_map()["selected"],
            format!("{panel}.item.1")
        );
        for (index, item) in list.c.as_ref().unwrap().iter().enumerate() {
            assert_eq!(item.id, format!("{panel}.item.{index}"));
            assert_eq!(item.p.as_ref().unwrap().as_map()["label"][0]["s"], "strong");
            assert!(!item.p.as_ref().unwrap().as_map().contains_key("detail"));
        }
        let detail = find_node(nodes, &format!("{panel}.detail")).unwrap();
        assert_eq!(
            detail.p.as_ref().unwrap().as_map()["spans"][0]["t"],
            "Full selected description 雪"
        );
        assert_eq!(detail.p.as_ref().unwrap().as_map()["wrap"], "word");
        drop(state);
        let (result, _) = shell
            .panel_input(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
        assert_eq!(result, PanelResult::Confirm(1));
        assert_eq!(shell.pending(), "unsent 雪 draft");
        assert!(shell.state.borrow().transcript.is_empty());
    }
}

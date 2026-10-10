//! Built-in reports and ordinary menus keep hierarchy, literals and input ownership.
use super::*;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

const STATUS: &str = "Provider       local\nModel          qa_model\nWorkspace      /synthetic/path\nSession        fixture\nSession cost   $0 (known subtotal; pricing uncertain)\nCost guardrails limit disabled · turn warning $1\nCache hit rate  9804 bp\nCache warming\nMode: off (each refresh is billable)\nStatus: Inactive\nRefreshes: 0 · cost: $0\nExtension-overridden refreshes: 0\nModel turns    754\nTool calls     949\nSkills         0 active / 17 discovered\n\nExtensions     5 ready / 6 discovered\n\nSecurity model: local agent with workspace trust gates\nEffect policy: full access (no sandbox and no approvals)\nOS isolation: none\nRepository trust: untrusted\nQueued reconfiguration: none";

#[test]
fn rich_fact_reports_style_every_status_section_and_preserve_no_color_and_draft() {
    for color in [ColorDepth::TrueColor, ColorDepth::Ansi16, ColorDepth::None] {
        for unicode in [true, false] {
            let theme = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
                true, unicode, color,
            ));
            let mut shell = InteractiveShell::test_shell_with_theme(theme);
            shell.prefill_editor("unsent 雪 draft".into());
            let caret = shell.state.borrow().editor.cursor();
            shell.show_status_text_with_telemetry(STATUS.into());
            let state = shell.state.borrow();
            let Some(ShellOverlay::Report(report)) = &state.overlay else {
                panic!("report")
            };
            let ReportBody::Markdown(_, source) = &report.body else {
                panic!("rich report")
            };
            for heading in [
                "Model and connection",
                "Session",
                "Spend and cache",
                "Cache warming",
                "Activity",
                "Extensions",
                "Safety and permissions",
                "Telemetry",
            ] {
                assert!(source.contains(&format!("## {heading}")), "{source}");
            }
            for label in [
                "Model",
                "Mode",
                "Extension-overridden refreshes",
                "OS isolation",
                "Repository trust",
                "Usage source",
                "Turn cost",
                "Throughput",
            ] {
                // Label punctuation is escaped, not reinterpreted as Markdown.
                let escaped =
                    report_document::facts_markdown("Fixture", &format!("{label}: sentinel"));
                assert!(
                    source.contains(escaped.trim().split(" sentinel").next().unwrap()),
                    "{label}: {source}"
                );
            }
            assert!(
                source.contains("98&#46;04&#37; &#40;9804 bp&#41;"),
                "{source}"
            );
            for width in [24, 40, 80, 120] {
                let rows = viewport::overlay_lines(&state, width, 2000);
                assert!(
                    rows.iter()
                        .all(|line| visible_width(line) <= usize::from(width)),
                    "{width}: {rows:?}"
                );
                let plain = strip_terminal_sequences(&rows.join("\n"));
                let compact = plain
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>();
                for value in [
                    "no sandbox and no approvals",
                    "pricing uncertain",
                    "98.04%",
                    "untrusted",
                ] {
                    let expected = value
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>();
                    assert!(compact.contains(&expected), "{width}: {plain}");
                }
                if color == ColorDepth::None {
                    assert!(rows.iter().all(|line| !line.contains('\x1b')), "{rows:?}");
                } else {
                    assert!(
                        rows.iter().any(|line| line.contains("\x1b[1m")),
                        "bold labels missing"
                    );
                }
            }
            drop(state);
            assert_eq!(
                shell.overlay_input(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
                OverlayInputResult::Closed
            );
            assert_eq!(shell.pending(), "unsent 雪 draft");
            assert_eq!(shell.state.borrow().editor.cursor(), caret);
            assert!(shell.state.borrow().transcript.is_empty());
        }
    }
}

#[test]
fn fact_values_are_literal_even_with_markdown_links_control_sequences_and_colons() {
    let text = "Model          qa: [not a link](https://example.test) **not bold** _literal_\nFile: /synthetic/雪_[draft].md\nNotice: \x1b[31mred\x1b[0m\x07";
    let source = report_document::facts_markdown("Session", text);
    assert!(
        source.starts_with("- **Model:** qa&#58;"),
        "value colon must not become the field boundary: {source}"
    );
    assert!(!source.contains('\x1b') && !source.contains('\x07'));
    let theme = crate::tui::theme::test_theme();
    let rendered = theme.rich_renderer().render(&parse_markdown(&source), 160);
    let plain = rendered
        .lines
        .iter()
        .map(|line| line.plain.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for literal in [
        "[not a link](https://example.test)",
        "**not bold**",
        "_literal_",
        "/synthetic/雪_[draft].md",
        "red",
    ] {
        assert!(plain.contains(literal), "{plain}");
    }
}

#[test]
fn accounting_records_stack_all_cells_totals_and_unknown_cost_without_clipping() {
    for (title, text, expected) in [
        ("Cost", "Session cost · $0.001\n\n  Turn  Model             Input tok/µ$  CacheR tok/µ$  CacheW tok/µ$  Output tok/µ$  Reason tok/µ$  Total µ$\n  ────  ─────  ────\n  1     qa-model          100/10        200/20          0/0            30/—            5/—             —\n  Total                   100/10        200/20          0/0            30/0            5/0             $0.001\n\nModel: qa-model", vec!["### Call 1", "### Totals", "Input tok/µ$", "CacheR tok/µ$", "CacheW tok/µ$", "Output tok/µ$", "Reason tok/µ$", "Total µ$", "Total spend", "30/—", "$0.001"]),
        ("Cache", "  Assistant entry    Expected  Cached  Missed  Waste      Cause\n  ────  ─────\n  fixture            1,000     800     200     $0.002     model changed, idle timeout", vec!["### Entry fixture", "Expected", "Cached", "Missed", "Waste", "Cause", "model changed, idle timeout"]),
    ] {
        let source = report_document::facts_markdown(title, text);
        let theme = crate::tui::theme::test_theme();
        let document = parse_markdown(&source);
        let plain = theme.rich_renderer().render(&document, 160).lines.into_iter().map(|line| line.plain).collect::<Vec<_>>().join("\n");
        for expected in expected {
            let expected = expected.trim_start_matches("### ");
            assert!(plain.contains(expected), "missing {expected}: {plain}");
        }
        for width in [24, 40, 80] {
            let rows = theme.rich_renderer().render(&document, width).lines;
            assert!(rows.iter().all(|line| visible_width(&line.plain) <= usize::from(width)), "{source}");
        }
    }
}

#[test]
fn help_commands_have_strong_labels_and_effective_settings_do_not_claim_saved_defaults() {
    let source = report_document::facts_markdown("Help", "octet help\nSlash commands:\n/status — Read status\n/settings images <on|off> — Display only");
    assert!(source.contains("## Slash commands"));
    assert!(source.contains("- **&#47;status:**"), "{source}");
    assert!(!source.contains("octet help"));
    let settings = report_document::facts_markdown("Settings", "octet settings\nConfigured model   fixture\nActive reasoning   high\nTheme              pi\nCache warming      off\nEditor padding     compiled theme layout\nProject trust is deliberately not persisted here: workspace trust comes from --workspace-trusted\nChange: /settings");
    for heading in [
        "Launch and session",
        "Display",
        "Billable refresh policy",
        "Editor layout",
        "Workspace trust",
        "Change a preference",
    ] {
        assert!(settings.contains(&format!("## {heading}")), "{settings}");
    }
    assert!(!settings.contains("Default model") && !settings.contains("Default reasoning"));
}

#[test]
fn ordinary_action_menus_stack_descriptions_keep_raw_indices_and_exclude_approvals() {
    for action in [
        PanelAction::SelectSettings(vec![
            commands::SettingsCommand::Theme(None),
            commands::SettingsCommand::Show,
        ]),
        PanelAction::SelectExtension(vec!["first".into(), "second".into()]),
        PanelAction::ProviderSetup(vec!["first".into(), "second".into()]),
    ] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.prefill_editor("unsent draft".into());
        shell.open_panel(Panel::SelectList {
            surface: OrdinarySurfaceMetadata::with_purpose("Menu", "Choose an action"),
            items: vec!["First action".into(), "Second action".into()],
            descriptions: vec![
                Some("first description".into()),
                Some("second description".into()),
            ],
            selected: 0,
            filter: String::new(),
            action,
        });
        let rows = render_panel(&shell.state.borrow(), 80);
        let plain = rows
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>();
        let label = plain
            .iter()
            .position(|line| line.contains("Second action"))
            .unwrap();
        assert!(plain[label + 1].contains("second description"), "{plain:?}");
        assert!(
            rows[label].contains("\x1b[1m"),
            "unselected action labels should be bold"
        );
        shell.panel_input(&Event::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        let (result, _) = shell
            .panel_input(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
        assert_eq!(result, PanelResult::Confirm(1));
        assert_eq!(shell.pending(), "unsent draft");
    }
}

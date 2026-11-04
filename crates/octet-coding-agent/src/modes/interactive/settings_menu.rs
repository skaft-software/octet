//! Synchronous settings panels; the interactive owner keeps driving input/runs.
//!
//! Open `root_panel` for `SettingsCommand::Menu`. A confirmed
//! `PanelAction::SelectSettings` index is already the original, unfiltered index.
//! In that menu-result path only, `Images(None)`, `DefaultModel(None)`,
//! `DefaultReasoning(None)`, and `TuiMode(None)` open their child panels rather than invoking the
//! explicit slash-command report/toggle behavior. `Theme(None)` opens the existing
//! theme picker, `Menu` is Back, and `Show` opens the existing diagnostic report.
//! Explicit values use the owner's existing safe display/deferred settings
//! dispatch. Escape retains ordinary SelectList cancellation (closes the menu).
//!
//! Catalog eligibility and supported portable levels are supplied by the owner;
//! these builders never discover models, resolve credentials, persist preferences,
//! or infer saved defaults from the active model/reasoning or launch config.

use crate::cli::QueueKind;
use crate::commands::SettingsCommand;
use crate::config::{queue_delivery_mode_label, ThinkingLevel, TuiMode};
use crate::tui::view::{OrdinarySurfaceLifecycle, OrdinarySurfaceMetadata, Panel, PanelAction};
use octet_agent::QueueDeliveryMode;
use octet_ai::ModelId;

type Row = (String, String, SettingsCommand);

fn panel(surface: OrdinarySurfaceMetadata, rows: Vec<Row>, selected: usize) -> Panel {
    let mut items = Vec::with_capacity(rows.len());
    let mut descriptions = Vec::with_capacity(rows.len());
    let mut commands = Vec::with_capacity(rows.len());
    for (label, description, command) in rows {
        items.push(label);
        descriptions.push(Some(description));
        commands.push(command);
    }
    Panel::SelectList {
        surface,
        items,
        descriptions,
        selected,
        filter: String::new(),
        action: PanelAction::SelectSettings(commands),
    }
}

fn back() -> Row {
    (
        "Back".into(),
        "Return to Settings without changing a preference".into(),
        SettingsCommand::Menu,
    )
}

/// Saved Pi steering and follow-up delivery modes, supplied by the owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct QueueModes {
    pub(super) steering: QueueDeliveryMode,
    pub(super) follow_up: QueueDeliveryMode,
}

impl QueueModes {
    pub(super) fn saved() -> Self {
        Self {
            steering: crate::cli::queue_mode(QueueKind::Steering),
            follow_up: crate::cli::queue_mode(QueueKind::FollowUp),
        }
    }
}

fn queue_description(kind: QueueKind) -> &'static str {
    match kind {
        QueueKind::Steering => {
            "Messages sent while the agent works: deliver one per turn, or all together"
        }
        QueueKind::FollowUp => {
            "Messages queued for after the agent finishes: run one at a time, or all together"
        }
    }
}

fn queue_title(kind: QueueKind) -> &'static str {
    match kind {
        QueueKind::Steering => "Steering mode",
        QueueKind::FollowUp => "Follow-up mode",
    }
}

/// Root destinations. `show_images` is effective display state, not a saved default.
pub(super) fn root_panel(show_images: bool, tui_mode: TuiMode, queues: QueueModes) -> Panel {
    panel(
        OrdinarySurfaceMetadata::with_purpose(
            "Settings",
            "Choose a display or new-session preference",
        ),
        vec![
            (
                "Theme".into(),
                "Preview and choose a terminal appearance".into(),
                SettingsCommand::Theme(None),
            ),
            (
                format!("Terminal screen ({})", tui_mode.as_str()),
                "Choose the saved fullscreen/inline preference; --tui-mode overrides it for this invocation".into(),
                SettingsCommand::TuiMode(None),
            ),
            (
                format!(
                    "{} ({})",
                    queue_title(QueueKind::Steering),
                    queue_delivery_mode_label(queues.steering)
                ),
                queue_description(QueueKind::Steering).into(),
                SettingsCommand::QueueMode(QueueKind::Steering, None),
            ),
            (
                format!(
                    "{} ({})",
                    queue_title(QueueKind::FollowUp),
                    queue_delivery_mode_label(queues.follow_up)
                ),
                queue_description(QueueKind::FollowUp).into(),
                SettingsCommand::QueueMode(QueueKind::FollowUp, None),
            ),
            (
                format!(
                    "Inline tool-result images ({})",
                    if show_images { "On" } else { "Off" }
                ),
                "Display only; not upload or attachment consent".into(),
                SettingsCommand::Images(None),
            ),
            (
                "Default model for new sessions".into(),
                "Save a catalog choice without switching this session".into(),
                SettingsCommand::DefaultModel(None),
            ),
            (
                "Default reasoning for new sessions".into(),
                "Save an available portable level without changing this session".into(),
                SettingsCommand::DefaultReasoning(None),
            ),
            (
                "Show effective settings".into(),
                "Read-only diagnostics, including launch and session facts".into(),
                SettingsCommand::Show,
            ),
        ],
        0,
    )
}

/// Fullscreen/inline choices plus Back; CLI mode remains authoritative per launch.
pub(super) fn tui_mode_panel(tui_mode: TuiMode) -> Panel {
    let rows = [TuiMode::Fullscreen, TuiMode::Inline]
        .into_iter()
        .map(|mode| {
            (
                format!(
                    "{}{}",
                    mode.as_str(),
                    if mode == tui_mode { " (current)" } else { "" },
                ),
                "Persisted interactive terminal mode; --tui-mode remains authoritative for this invocation".into(),
                SettingsCommand::TuiMode(Some(mode)),
            )
        })
        .chain(std::iter::once(back()))
        .collect();
    panel(
        OrdinarySurfaceMetadata::with_purpose(
            "Terminal screen",
            "Choose fullscreen or inline rendering for the next interactive launch",
        ),
        rows,
        match tui_mode {
            TuiMode::Fullscreen => 0,
            TuiMode::Inline => 1,
        },
    )
}

/// One-at-a-time/all choices plus Back; the saved mode applies to the next run.
pub(super) fn queue_mode_panel(kind: QueueKind, current: QueueDeliveryMode) -> Panel {
    let rows = [QueueDeliveryMode::OneAtATime, QueueDeliveryMode::All]
        .into_iter()
        .map(|mode| {
            (
                format!(
                    "{}{}",
                    queue_delivery_mode_label(mode),
                    if mode == current { " (current)" } else { "" },
                ),
                match (kind, mode) {
                    (QueueKind::Steering, QueueDeliveryMode::OneAtATime) => {
                        "Deliver one queued steering message at each turn boundary"
                    }
                    (QueueKind::Steering, QueueDeliveryMode::All) => {
                        "Deliver every queued steering message at the next turn boundary"
                    }
                    (QueueKind::FollowUp, QueueDeliveryMode::OneAtATime) => {
                        "Run each queued follow-up as its own turn"
                    }
                    (QueueKind::FollowUp, QueueDeliveryMode::All) => {
                        "Send every queued follow-up together in one turn"
                    }
                }
                .into(),
                SettingsCommand::QueueMode(kind, Some(mode)),
            )
        })
        .chain(std::iter::once(back()))
        .collect();
    panel(
        OrdinarySurfaceMetadata::with_purpose(queue_title(kind), queue_description(kind)),
        rows,
        usize::from(current == QueueDeliveryMode::All),
    )
}

/// Explicit On/Off values plus Back; no toggle is inferred at confirmation time.
pub(super) fn images_panel(show_images: bool) -> Panel {
    let rows = [true, false]
        .into_iter()
        .map(|enabled| {
            (
                format!(
                    "{}{}",
                    if enabled { "On" } else { "Off" },
                    if enabled == show_images {
                        " (current)"
                    } else {
                        ""
                    },
                ),
                "Inline tool-result display only; not upload or attachment consent".into(),
                SettingsCommand::Images(Some(enabled)),
            )
        })
        .chain(std::iter::once(back()))
        .collect();
    panel(
        OrdinarySurfaceMetadata::with_purpose(
            "Inline tool-result images",
            "Choose display behavior on compatible terminals",
        ),
        rows,
        usize::from(!show_images),
    )
}

/// Read-only, already eligible `(display label, model ID)` catalog choices.
/// IDs remain exact and are searchable in descriptions, independently of labels.
pub(super) fn default_model_panel(choices: &[(String, ModelId)]) -> Panel {
    let mut surface = OrdinarySurfaceMetadata::with_purpose(
        "Default model for new sessions",
        "Save a model preference; the active session is not switched",
    );
    if choices.is_empty() {
        surface.lifecycle = OrdinarySurfaceLifecycle::empty("No available catalog models");
    }
    let rows = choices
        .iter()
        .map(|(label, id)| {
            (
                label.clone(),
                format!("{} · for new sessions only", id.0),
                SettingsCommand::DefaultModel(Some(id.0.clone())),
            )
        })
        .chain(std::iter::once(back()))
        .collect();
    panel(surface, rows, 0)
}

/// Existing, available portable levels, in the owner's order. No budgets or
/// provider-specific controls are manufactured by this panel.
pub(super) fn default_reasoning_panel(levels: &[ThinkingLevel]) -> Panel {
    let mut surface = OrdinarySurfaceMetadata::with_purpose(
        "Default reasoning for new sessions",
        "Save a reasoning preference; the active session is not changed",
    );
    if levels.is_empty() {
        surface.lifecycle =
            OrdinarySurfaceLifecycle::empty("No available portable reasoning levels");
    }
    let rows = levels
        .iter()
        .map(|level| {
            (
                level.label().into(),
                "For new sessions only; support depends on the selected model".into(),
                SettingsCommand::DefaultReasoning(Some(level.label().into())),
            )
        })
        .chain(std::iter::once(back()))
        .collect();
    panel(surface, rows, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::view::{InteractiveShell, PanelResult};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn confirm(shell: &mut InteractiveShell) -> (usize, SettingsCommand) {
        let (PanelResult::Confirm(index), PanelAction::SelectSettings(commands)) =
            shell.panel_input(&key(KeyCode::Enter)).expect("selection")
        else {
            panic!("settings selection expected");
        };
        assert!(!shell.has_panel());
        (index, commands[index].clone())
    }

    fn filter(shell: &mut InteractiveShell, query: &str) {
        for character in query.chars() {
            assert!(shell.panel_input(&key(KeyCode::Char(character))).is_none());
        }
    }

    fn parts(panel: Panel) -> (Vec<String>, Vec<SettingsCommand>, usize) {
        let Panel::SelectList {
            items,
            descriptions,
            selected,
            filter,
            action: PanelAction::SelectSettings(commands),
            ..
        } = panel
        else {
            panic!("ordinary settings panel expected");
        };
        assert_eq!(items.len(), descriptions.len());
        assert_eq!(items.len(), commands.len());
        assert!(filter.is_empty());
        (items, commands, selected)
    }

    #[test]
    fn root_has_only_the_approved_destinations_and_no_inferred_defaults() {
        let (items, commands, selected) = parts(root_panel(
            false,
            TuiMode::Fullscreen,
            QueueModes::default(),
        ));
        assert_eq!(
            items,
            vec![
                "Theme",
                "Terminal screen (fullscreen)",
                "Steering mode (one-at-a-time)",
                "Follow-up mode (one-at-a-time)",
                "Inline tool-result images (Off)",
                "Default model for new sessions",
                "Default reasoning for new sessions",
                "Show effective settings",
            ],
        );
        assert_eq!(
            commands,
            vec![
                SettingsCommand::Theme(None),
                SettingsCommand::TuiMode(None),
                SettingsCommand::QueueMode(QueueKind::Steering, None),
                SettingsCommand::QueueMode(QueueKind::FollowUp, None),
                SettingsCommand::Images(None),
                SettingsCommand::DefaultModel(None),
                SettingsCommand::DefaultReasoning(None),
                SettingsCommand::Show,
            ],
        );
        assert_eq!(selected, 0);
        assert_eq!(
            parts(root_panel(true, TuiMode::Fullscreen, QueueModes::default())).0[4],
            "Inline tool-result images (On)"
        );
    }

    #[test]
    fn queue_mode_choices_preselect_the_saved_mode_and_offer_back() {
        for kind in [QueueKind::Steering, QueueKind::FollowUp] {
            for mode in [QueueDeliveryMode::OneAtATime, QueueDeliveryMode::All] {
                let (items, commands, selected) = parts(queue_mode_panel(kind, mode));
                assert_eq!(
                    commands[selected],
                    SettingsCommand::QueueMode(kind, Some(mode))
                );
                assert!(items[selected].ends_with(" (current)"));
                assert_eq!(commands.last(), Some(&SettingsCommand::Menu));
            }
        }
        let modes = QueueModes {
            steering: QueueDeliveryMode::All,
            follow_up: QueueDeliveryMode::OneAtATime,
        };
        let items = parts(root_panel(false, TuiMode::Fullscreen, modes)).0;
        assert_eq!(items[2], "Steering mode (all)");
        assert_eq!(items[3], "Follow-up mode (one-at-a-time)");
    }

    #[test]
    fn tui_mode_choices_preselect_the_effective_mode() {
        for mode in [TuiMode::Fullscreen, TuiMode::Inline] {
            let (_, commands, selected) = parts(tui_mode_panel(mode));
            assert_eq!(commands[selected], SettingsCommand::TuiMode(Some(mode)));
            assert_eq!(commands.last(), Some(&SettingsCommand::Menu));
        }
    }

    #[test]
    fn images_preselects_effective_display_and_maps_explicit_values() {
        for enabled in [false, true] {
            let (_, commands, selected) = parts(images_panel(enabled));
            assert_eq!(commands[selected], SettingsCommand::Images(Some(enabled)));
            assert_eq!(commands.last(), Some(&SettingsCommand::Menu));
            let mut shell = InteractiveShell::test_shell();
            shell.open_panel(images_panel(enabled));
            assert_eq!(
                confirm(&mut shell).1,
                SettingsCommand::Images(Some(enabled))
            );
        }
    }

    #[test]
    fn filtering_preserves_raw_model_indices_and_exact_catalog_ids() {
        let choices = vec![
            ("Alpha".into(), ModelId("custom/alpha".into())),
            ("Friendly label".into(), ModelId("custom/beta".into())),
        ];
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("unsent draft".into());
        shell.open_panel(default_model_panel(&choices));
        filter(&mut shell, "custom/beta");
        assert_eq!(
            confirm(&mut shell),
            (1, SettingsCommand::DefaultModel(Some("custom/beta".into()))),
        );
        assert_eq!(shell.pending(), "unsent draft");
        assert_eq!(choices[1].1 .0, "custom/beta");
    }

    #[test]
    fn reasoning_choices_are_only_the_supplied_portable_levels() {
        let (_, commands, _) = parts(default_reasoning_panel(&[
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Max,
        ]));
        assert_eq!(
            commands,
            vec![
                SettingsCommand::DefaultReasoning(Some("off".into())),
                SettingsCommand::DefaultReasoning(Some("low".into())),
                SettingsCommand::DefaultReasoning(Some("max".into())),
                SettingsCommand::Menu,
            ],
        );
    }

    #[test]
    fn child_back_and_escape_use_ordinary_selection_and_cancellation() {
        for panel in [
            tui_mode_panel(TuiMode::Fullscreen),
            images_panel(false),
            default_model_panel(&[]),
            default_reasoning_panel(&[ThinkingLevel::High]),
        ] {
            let mut shell = InteractiveShell::test_shell();
            shell.open_panel(panel);
            filter(&mut shell, "back");
            assert_eq!(confirm(&mut shell).1, SettingsCommand::Menu);
            shell.open_panel(root_panel(
                false,
                TuiMode::Fullscreen,
                QueueModes::default(),
            ));
            assert!(matches!(
                shell.panel_input(&key(KeyCode::Esc)),
                Some((PanelResult::Cancel, PanelAction::SelectSettings(_))),
            ));
            assert!(!shell.has_panel());
        }
    }

    #[test]
    fn empty_sources_offer_back_without_fabricating_preferences() {
        for panel in [default_model_panel(&[]), default_reasoning_panel(&[])] {
            let Panel::SelectList { surface, .. } = &panel else {
                panic!("ordinary settings panel expected");
            };
            assert!(matches!(
                surface.lifecycle,
                OrdinarySurfaceLifecycle::Empty(_)
            ));
            let (items, commands, selected) = parts(panel);
            assert_eq!(items, vec!["Back"]);
            assert_eq!(commands, vec![SettingsCommand::Menu]);
            assert_eq!(selected, 0);
        }
    }

    #[test]
    fn no_filter_matches_keeps_the_menu_open_until_cancelled() {
        let mut shell = InteractiveShell::test_shell();
        shell.open_panel(root_panel(
            false,
            TuiMode::Fullscreen,
            QueueModes::default(),
        ));
        filter(&mut shell, "does-not-match");
        assert!(shell.panel_input(&key(KeyCode::Enter)).is_none());
        assert!(shell.has_panel());
        assert!(matches!(
            shell.panel_input(&key(KeyCode::Esc)),
            Some((PanelResult::Cancel, PanelAction::SelectSettings(_))),
        ));
    }
}

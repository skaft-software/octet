//! Paint the adapter's validated startup template using the native draft.
use super::*;
use crate::extensions::pi_chrome::{Field, Part};
use octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement as Placement;

pub(super) fn apply(
    state: &ShellState,
    width: u16,
    chrome: &mut shell_chrome::ShellChrome,
) -> bool {
    let Some(snapshot) = &state.pi_chrome else {
        return false;
    };
    if state.panel.is_some()
        || state.overlay.is_some()
        || state.tool_input_prompt.is_some()
        || state.transcript_search_active()
        || (snapshot.columns, snapshot.rows) != state.size
        || width != snapshot.columns
    {
        return false;
    }
    if !state.model.is_empty()
        && snapshot
            .defaults
            .get("native_model")
            .is_some_and(|model| model != &state.model)
    {
        return false;
    }
    let configured = |field: &str| snapshot.defaults.get(field).cloned().unwrap_or_default();
    let remote = &state.extension_ui.remote;
    let ready = |placement| {
        remote
            .mount(placement)
            .is_some_and(|mount| !remote.lines(mount, width, state.size.1).is_empty())
    };
    // Reveal the live pair atomically. A mount without its first frame is not
    // readiness; otherwise opening the editor would briefly erase the cache.
    if (snapshot.editor.is_none() || ready(Placement::Editor))
        && (snapshot.footer.is_none() || ready(Placement::Footer))
    {
        return false;
    }
    let reserved = chrome.header.len()
        + chrome.error.len()
        + chrome.pending.len()
        + chrome.subagents.len()
        + chrome.suggestions.len()
        + chrome.extension_above.len()
        + snapshot.footer.as_ref().map_or(0, Vec::len)
        + 2;
    if reserved >= usize::from(state.size.1) {
        return false;
    }
    let original = chrome.composer.len() + chrome.extension_below.len();
    if let Some(editor) = &snapshot.editor {
        let cells = usize::from(width).saturating_sub(editor.padding * 2).max(1);
        let projection = ComposerEditorProjection::new(
            state.editor.text(),
            state.editor.cursor(),
            editor.text_width,
        );
        let count = editor
            .max_rows
            .min(projection.line_count())
            .min(usize::from(state.size.1) - reserved);
        let start = projection
            .cursor_row()
            .saturating_sub(count.saturating_sub(1));
        let mut lines = vec![editor.top.clone()];
        for index in start..(start + count).min(projection.line_count()) {
            let row = projection.visible_row(index, sexy_tui_rs::CURSOR_MARKER, cells);
            let padding = cells.saturating_sub(visible_width(&row));
            lines.push(format!(
                "{}{}{}{}",
                " ".repeat(editor.padding),
                row,
                " ".repeat(padding),
                " ".repeat(editor.padding)
            ));
        }
        lines.push(editor.bottom.clone());
        chrome.composer = lines;
    } else if crate::tui::composer_surface::status_footer_visible(state, width) {
        chrome.composer.pop();
    }
    if let Some(footer) = &snapshot.footer {
        chrome.extension_below = footer
            .iter()
            .map(|line| {
                let mut output = String::new();
                for part in line {
                    match part {
                        Part::Text { text } => output.push_str(text),
                        Part::Field { field } => {
                            let value = match field {
                                Field::Model => {
                                    if state.model.is_empty() {
                                        configured("model")
                                    } else {
                                        state
                                            .model
                                            .rsplit('/')
                                            .next()
                                            .unwrap_or_default()
                                            .to_owned()
                                    }
                                }
                                Field::ModelName => {
                                    if snapshot.defaults.contains_key("model_name") {
                                        configured("model_name")
                                    } else if state.model_display.is_empty() {
                                        state.model.clone()
                                    } else {
                                        state.model_display.clone()
                                    }
                                }
                                Field::Provider => configured("provider"),
                                Field::Reasoning => {
                                    if state.reasoning.is_empty() {
                                        configured("reasoning")
                                    } else {
                                        state.reasoning.clone()
                                    }
                                }
                                Field::Workspace => {
                                    snapshot.workspace.to_string_lossy().into_owned()
                                }
                                Field::HomeWorkspace => {
                                    let path = snapshot.workspace.to_string_lossy();
                                    dirs::home_dir()
                                        .and_then(|home| {
                                            path.strip_prefix(home.to_str()?)
                                                .map(|tail| format!("~{tail}"))
                                        })
                                        .unwrap_or_else(|| path.into_owned())
                                }
                                Field::Zero => "0".into(),
                            };
                            output.push_str(&sanitize_for_terminal(&value));
                        }
                    }
                }
                format!(
                    "{}\x1b[0m",
                    sexy_tui_rs::truncate_to_width(&output, usize::from(width), Some(""))
                )
            })
            .collect();
    }
    let replacement = chrome.composer.len() + chrome.extension_below.len();
    chrome.transcript_rows = chrome
        .transcript_rows
        .saturating_add(original)
        .saturating_sub(replacement);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cached_chrome_paints_first_frame_and_keeps_early_edits_in_native_draft() {
        let snapshot = serde_json::from_value(json!({
            "schema":"octet.pi-chrome/v1", "agent":"/fixture", "workspace":"/fixture",
            "trusted":false, "columns":80, "rows":24, "identity":[],
            "editor":{"top":"\u{1b}[35m────────────────────────────────────────────────────────────────────────────────\u{1b}[39m",
                "bottom":"────────────────────────────────────────────────────────────────────────────────", "padding":0, "text_width":79, "max_rows":7},
            "footer":[[{"field":"model"}, {"text":" · "}, {"field":"zero"}, {"text":" context"}]]
        })).unwrap();
        let mut state = ShellState {
            size: (80, 24),
            startup_pending: true,
            model: "custom/custom/mock-1".into(),
            pi_chrome: Some(Arc::new(snapshot)),
            ..ShellState::default()
        };
        let first = super::super::shell_chrome::render_startup_surface(&state, 80, true);
        assert!(first.iter().any(|line| line.contains("\x1b[35m─")));
        assert!(first.iter().any(|line| line.contains("mock-1 · 0 context")));
        state.editor.set_text("early draft");
        let edited = super::super::shell_chrome::render_startup_surface(&state, 80, true);
        assert!(edited.iter().any(|line| line.contains("early draft")));
        assert_eq!(first.len(), edited.len());
        assert_eq!(
            first.iter().position(|line| line.contains("\x1b[35m─")),
            edited.iter().position(|line| line.contains("\x1b[35m─"))
        );
        state.editor.set_text("x".repeat(80));
        let wrapped = super::super::shell_chrome::render_startup_surface(&state, 80, true);
        assert_eq!(
            wrapped
                .iter()
                .position(|line| line.contains("\x1b[35m─"))
                .unwrap()
                + 1,
            first
                .iter()
                .position(|line| line.contains("\x1b[35m─"))
                .unwrap(),
            "Pi reserves a cursor cell when wrapping without padding"
        );
        assert!(wrapped.iter().any(|line| line.contains(&"x".repeat(79))));
        // An open mount is not a frame. Nor may one live placement expose a
        // half-native/half-Pi layout while the other placement is still cold.
        for (placement, id) in [(Placement::Editor, "editor"), (Placement::Footer, "footer")] {
            state
                .extension_ui
                .remote
                .mounts
                .push(crate::extensions::remote_ui::MountView {
                    id: id.into(),
                    surface_id: id.into(),
                    title: id.into(),
                    placement,
                    columns: 80,
                    rows: 24,
                    mouse_capture: false,
                    native_editor: placement == Placement::Editor,
                });
            Arc::make_mut(&mut state.extension_ui.remote.components)
                .register_component(id)
                .unwrap();
        }
        let mut chrome = builtin_shell_chrome::shell_chrome(&state, 80, Instant::now());
        assert!(apply(&state, 80, &mut chrome));
        Arc::make_mut(&mut state.extension_ui.remote.components)
            .store_render("footer", 80, vec!["live footer".into()])
            .unwrap();
        assert!(apply(&state, 80, &mut chrome));
        Arc::make_mut(&mut state.extension_ui.remote.components)
            .store_render("editor", 80, vec!["live editor".into()])
            .unwrap();
        assert!(
            !apply(&state, 80, &mut chrome),
            "only the complete live pair retires the snapshot"
        );
        assert_eq!(state.editor.text(), "x".repeat(80));
        state.size = (81, 24);
        let mut chrome = super::super::shell_chrome::shell_chrome(&state, 81, Instant::now());
        assert!(
            !apply(&state, 81, &mut chrome),
            "resized geometry uses native fallback"
        );
    }
}

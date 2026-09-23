//! Composer-adjacent slash, mention, and queued-steering overlays.

use sexy_tui_rs::{strip_terminal_sequences, truncate_to_width, visible_width};

use super::{
    activity_elbow, fit_line, fit_prioritized_footer, join_ordinary_metadata,
    sanitize_ordinary_surface_cell, semantic_separator, subdued_text, FooterSegment, ShellState,
    ACTIVITY_DETAIL_INDENT,
};
use crate::commands;
use crate::tui::composer;
use crate::tui::keymap::keybindings;

/// Provenance remains semantic data until the display projection joins it to
/// untrusted metadata. Raw command identity never includes this label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SlashSuggestionProvenance {
    Builtin,
    Prompt,
    Skill,
    Extension,
}

impl SlashSuggestionProvenance {
    fn label(self) -> Option<&'static str> {
        match self {
            Self::Builtin => None,
            Self::Prompt => Some("prompt"),
            Self::Skill => Some("skill"),
            Self::Extension => Some("extension"),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct InputSlashSuggestion {
    /// Raw command identity used by selection and completion.
    pub(super) name: String,
    /// Raw descriptive metadata projected only at the terminal boundary.
    pub(super) description: String,
    pub(super) argument_hint: Option<String>,
    pub(super) provenance: SlashSuggestionProvenance,
    pub(super) accepts_argument: bool,
}

pub(super) fn input_slash_suggestions(state: &ShellState) -> Vec<InputSlashSuggestion> {
    let Some(query) = state.editor.text().strip_prefix('/') else {
        return Vec::new();
    };
    if query.contains(char::is_whitespace) || query.contains('\n') {
        return Vec::new();
    }
    let mut suggestions = commands::slash_suggestions(state.editor.text())
        .into_iter()
        .map(|command| InputSlashSuggestion {
            name: command.name.to_owned(),
            description: command.description.to_owned(),
            argument_hint: None,
            provenance: SlashSuggestionProvenance::Builtin,
            accepts_argument: command.accepts_argument,
        })
        .collect::<Vec<_>>();
    for template in state
        .prompt_templates
        .iter()
        .filter(|template| template.name.starts_with(query))
    {
        if suggestions
            .iter()
            .any(|suggestion| suggestion.name == template.name)
        {
            continue;
        }
        suggestions.push(InputSlashSuggestion {
            name: template.name.clone(),
            description: template.description.clone(),
            argument_hint: template.argument_hint.clone(),
            provenance: SlashSuggestionProvenance::Prompt,
            accepts_argument: true,
        });
    }
    for (name, description) in state
        .skill_commands
        .iter()
        .filter(|(name, _)| name.starts_with(query))
    {
        if suggestions
            .iter()
            .any(|suggestion| suggestion.name == *name)
        {
            continue;
        }
        suggestions.push(InputSlashSuggestion {
            name: name.clone(),
            description: description.clone(),
            argument_hint: None,
            provenance: SlashSuggestionProvenance::Skill,
            accepts_argument: true,
        });
    }
    for (name, description) in state
        .extension_commands
        .iter()
        .filter(|(name, _)| name.starts_with(query))
    {
        if suggestions
            .iter()
            .any(|suggestion| suggestion.name == *name)
        {
            continue;
        }
        suggestions.push(InputSlashSuggestion {
            name: name.clone(),
            description: description.clone(),
            argument_hint: None,
            provenance: SlashSuggestionProvenance::Extension,
            accepts_argument: true,
        });
    }
    suggestions
}

fn suggestion_key_hint(state: &ShellState, key: &str, label: &str) -> String {
    format!(
        "{} {}",
        state.theme.bold(&state.theme.fg("model_accent", key)),
        state.theme.fg("muted", label)
    )
}

fn suggestion_separator(state: &ShellState) -> String {
    state.theme.fg("muted", semantic_separator(&state.theme))
}

/// Convert untrusted command metadata into one terminal-safe display cell
/// before it contributes to width, clipping, or trusted theme styling.
fn slash_suggestion_display_cell(state: &ShellState, value: &str) -> String {
    sanitize_ordinary_surface_cell(value, state.theme.unicode())
}

fn slash_suggestion_display_label(state: &ShellState, command: &InputSlashSuggestion) -> String {
    let name = slash_suggestion_display_cell(state, &command.name);
    let argument_hint = command
        .argument_hint
        .as_deref()
        .map(|hint| slash_suggestion_display_cell(state, hint))
        .map(|hint| format!(" {hint}"))
        .unwrap_or_default();
    format!("/{name}{argument_hint}")
}

fn slash_suggestion_display_description(
    state: &ShellState,
    command: &InputSlashSuggestion,
) -> String {
    let description = slash_suggestion_display_cell(state, &command.description);
    command
        .provenance
        .label()
        .map_or(description.clone(), |label| {
            join_ordinary_metadata(&state.theme, &[label, &description])
        })
}

fn truncate_suggestion_display(value: &str, width: usize, ellipsis: &str) -> String {
    // Input is a plain, terminal-safe cell. The ANSI-aware truncator only adds
    // a trusted reset when clipping; strip that synthetic reset before styling
    // so no-colour renderers never receive an escape sequence.
    strip_terminal_sequences(&truncate_to_width(value, width, Some(ellipsis)))
}

fn slash_suggestion_footer(
    state: &ShellState,
    width: u16,
    start: usize,
    end: usize,
    total: usize,
    visible_rows: usize,
) -> String {
    let scope = if total > visible_rows {
        let range_separator = if state.theme.unicode() { "–" } else { "-" };
        format!(
            "commands {}{range_separator}{end}/{total}",
            start.saturating_add(1)
        )
    } else {
        "commands".to_owned()
    };
    let (navigation_key, select_key) = if state.theme.unicode() {
        ("↑↓", "↵")
    } else {
        ("up/down", "enter")
    };
    let separator = suggestion_separator(state);
    let mut segments = [
        // Scope is visually first but is the least useful segment at compact
        // widths, so its lower rank drops before the optional action tail.
        FooterSegment::optional(state.theme.fg("muted", &scope), 0),
        FooterSegment::primary(suggestion_key_hint(state, navigation_key, "navigate")),
        FooterSegment::optional(suggestion_key_hint(state, select_key, "select"), 2),
        FooterSegment::optional(suggestion_key_hint(state, "esc", "close"), 1),
    ];
    fit_prioritized_footer("  ", &separator, &mut segments, width)
}

pub(super) fn render_slash_suggestions(
    state: &ShellState,
    width: u16,
    max_rows: usize,
) -> Vec<String> {
    if state.slash_popup_dismissed || max_rows < 2 {
        return Vec::new();
    }
    let suggestions = input_slash_suggestions(state);
    if suggestions.is_empty() {
        return Vec::new();
    }

    let layout = crate::tui::layout::PresentationLayout::new(&state.theme, width);
    let popup_width = layout.content_width;
    let popup_prefix = " ".repeat(usize::from(layout.inset));

    // Keep one compact hint row below the choices. Moving the metadata to the
    // footer makes autocomplete read as an inline continuation of the composer
    // rather than a second panel with its own heading.
    let item_rows = max_rows.saturating_sub(1).max(1);
    let selected = state
        .slash_selection
        .min(suggestions.len().saturating_sub(1));
    let max_start = suggestions.len().saturating_sub(item_rows);
    let mut start = state.slash_scroll.min(max_start);
    if selected < start {
        start = selected;
    } else if selected >= start.saturating_add(item_rows) {
        start = selected + 1 - item_rows;
    }
    start = start.min(max_start);
    let end = start.saturating_add(item_rows).min(suggestions.len());

    let marker = state.theme.glyph("prompt");
    let marker_width = visible_width(marker).max(1);
    let label_width = suggestions[start..end]
        .iter()
        .map(|command| slash_suggestion_display_label(state, command))
        .map(|label| visible_width(&label))
        .max()
        .unwrap_or(1)
        .min(30)
        .min(
            usize::from(popup_width)
                .saturating_sub(marker_width + 1)
                .max(1),
        );
    let mut lines = Vec::with_capacity(end.saturating_sub(start) + 1);
    for (index, command) in suggestions[start..end].iter().enumerate() {
        let absolute = start + index;
        let selected_row = absolute == selected;
        let prefix = if selected_row {
            marker.to_owned()
        } else {
            " ".repeat(marker_width)
        };
        let display_label = slash_suggestion_display_label(state, command);
        let label =
            truncate_suggestion_display(&display_label, label_width, state.theme.glyph("ellipsis"));
        let label = format!(
            "{label}{}",
            " ".repeat(label_width.saturating_sub(visible_width(&label)))
        );
        let choice = format!("{prefix} {label}");
        let choice = if selected_row {
            state.theme.bold(&state.theme.fg("model_accent", &choice))
        } else {
            state.theme.fg("foreground", &choice)
        };
        let fixed_width = marker_width + 1 + label_width;
        let description_width = usize::from(popup_width).saturating_sub(fixed_width + 2);
        let display_description = slash_suggestion_display_description(state, command);
        let description = truncate_suggestion_display(
            &display_description,
            description_width,
            state.theme.glyph("ellipsis"),
        );
        let row = if description.is_empty() {
            choice
        } else {
            format!("{choice}  {}", state.theme.fg("muted", &description))
        };
        lines.push(fit_line(
            &format!("{popup_prefix}{}", fit_line(&row, popup_width)),
            width,
        ));
    }
    let footer =
        slash_suggestion_footer(state, popup_width, start, end, suggestions.len(), item_rows);
    lines.push(fit_line(&format!("{popup_prefix}{footer}"), width));
    lines
}

/// One bounded candidate list shared by painting, arrow navigation, and Tab.
/// The visible window is smaller than this list and follows the selected row.
pub(super) fn input_path_suggestions(state: &ShellState) -> Vec<composer::PathSuggestion> {
    const MAX_MATCHES: usize = 100;
    if state.editor.cursor() != state.editor.text().len() {
        return Vec::new();
    }
    let Some(root) = state.workspace.as_ref() else {
        return Vec::new();
    };
    if let Some(query) = composer::active_mention(state.editor.text()) {
        if composer::is_path_query(query) {
            return composer::path_matches(root, query, MAX_MATCHES);
        }
        return state.file_index.as_ref().map_or_else(Vec::new, |files| {
            composer::mention_matches(files, query, MAX_MATCHES)
                .into_iter()
                .map(|completion| composer::PathSuggestion {
                    completion: completion.to_owned(),
                    path: root.join(completion),
                    is_dir: false,
                })
                .collect()
        });
    }
    composer::active_path(state.editor.text()).map_or_else(Vec::new, |query| {
        composer::path_matches(root, query, MAX_MATCHES)
    })
}

fn render_path_suggestions(state: &ShellState, width: u16, max_rows: usize) -> Vec<String> {
    if max_rows < 2 {
        return Vec::new();
    }
    let matches = input_path_suggestions(state);
    if matches.is_empty() {
        return Vec::new();
    }
    let heading_label = if composer::active_mention(state.editor.text())
        .is_some_and(|query| !composer::is_path_query(query))
    {
        "project files"
    } else {
        "paths"
    };

    let item_rows = max_rows.saturating_sub(1).min(5);
    let selected = state.path_selection.min(matches.len() - 1);
    let start = selected.saturating_sub(item_rows - 1);
    let marker = state.theme.glyph("prompt");
    let marker_width = visible_width(marker).max(1);
    let available_width = usize::from(width)
        .saturating_sub(2 + marker_width + 1)
        .max(1);
    let mut lines = Vec::with_capacity(item_rows.saturating_add(1));
    for (index, suggestion) in matches.into_iter().enumerate().skip(start).take(item_rows) {
        let safe_path =
            sanitize_ordinary_surface_cell(&suggestion.completion, state.theme.unicode());
        let path =
            truncate_suggestion_display(&safe_path, available_width, state.theme.glyph("ellipsis"));
        let prefix = if index == selected {
            marker.to_owned()
        } else {
            " ".repeat(marker_width)
        };
        let choice = format!("{prefix} {path}");
        let choice = if index == selected {
            state.theme.bold(&state.theme.fg("model_accent", &choice))
        } else {
            state.theme.fg("muted", &choice)
        };
        lines.push(fit_line(&format!("  {choice}"), width));
    }

    let separator = suggestion_separator(state);
    let mut segments = [
        FooterSegment::optional(state.theme.fg("muted", heading_label), 0),
        FooterSegment::primary(suggestion_key_hint(state, "tab", "complete")),
        FooterSegment::optional(
            suggestion_key_hint(
                state,
                if state.theme.unicode() {
                    "↑↓"
                } else {
                    "up/down"
                },
                "navigate",
            ),
            1,
        ),
    ];
    lines.push(fit_prioritized_footer(
        "  ",
        &separator,
        &mut segments,
        width,
    ));
    lines
}

fn render_extension_autocomplete(state: &ShellState, width: u16, max_rows: usize) -> Vec<String> {
    if max_rows < 2 || !super::normal_editor_focused(state) {
        return Vec::new();
    }
    let Some(overlay) = state.extension_autocomplete.as_ref() else {
        return Vec::new();
    };
    if overlay.items.is_empty()
        || overlay.revision != state.editor.revision()
        || overlay.text != state.editor.text()
        || overlay.cursor != state.editor.cursor()
    {
        return Vec::new();
    }
    let item_rows = max_rows.saturating_sub(1).min(5);
    let marker = state.theme.glyph("prompt");
    let marker_width = visible_width(marker).max(1);
    let available = usize::from(width).saturating_sub(marker_width + 5).max(1);
    let mut lines = Vec::with_capacity(item_rows.saturating_add(1));
    for (index, item) in overlay.items.iter().take(item_rows).enumerate() {
        let prefix = if index == 0 {
            marker.to_owned()
        } else {
            " ".repeat(marker_width)
        };
        let label = super::sanitize_for_terminal(&item.label);
        let label = truncate_to_width(&label, available, None);
        let choice = format!("{prefix} {label}");
        let choice = if index == 0 {
            state.theme.bold(&state.theme.fg("model_accent", &choice))
        } else {
            state.theme.fg("foreground", &choice)
        };
        let description = item
            .description
            .as_deref()
            .map(super::sanitize_for_terminal)
            .filter(|description| !description.is_empty())
            .map(|description| truncate_to_width(&description, available / 2, None));
        let line = description.map_or(choice.clone(), |description| {
            format!("{choice}  {}", state.theme.fg("muted", &description))
        });
        lines.push(fit_line(&format!("  {line}"), width));
    }
    let separator = suggestion_separator(state);
    lines.push(fit_line(
        &format!(
            "  {}{separator}{}",
            state.theme.fg("muted", "extension suggestions"),
            suggestion_key_hint(state, "tab", "accept")
        ),
        width,
    ));
    lines
}

pub(super) fn render_input_suggestions(
    state: &ShellState,
    width: u16,
    max_rows: usize,
) -> Vec<String> {
    if state.transcript_search_active() {
        return Vec::new();
    }
    let extension = render_extension_autocomplete(state, width, max_rows);
    if !extension.is_empty() {
        return extension;
    }
    let slash = render_slash_suggestions(state, width, max_rows);
    if slash.is_empty() {
        render_path_suggestions(state, width, max_rows)
    } else {
        slash
    }
}

fn steering_preview_text(state: &ShellState, message: &str) -> String {
    let marker = if state.theme.unicode() {
        " ↵ "
    } else {
        " / "
    };
    super::sanitize_for_terminal(message).replace('\n', marker)
}

fn clipped_steering_content(state: &ShellState, content: &str, width: usize) -> String {
    let suffix = if state.theme.unicode() {
        " …"
    } else {
        " ..."
    };
    let suffix_width = visible_width(suffix);
    if width <= suffix_width {
        return truncate_to_width(suffix.trim_start(), width, Some(""));
    }
    let body = truncate_to_width(content, width - suffix_width, Some(""));
    format!("{}{suffix}", body.trim_end())
}

/// Namespaced binding the compiled default declares for restoring a queued
/// message (`crates/octet-coding-agent/src/tui/keymap/keybindings.rs`).
const QUEUED_EDIT_BINDING: &str = "app.message.dequeue";

/// TODO(resolved-binding): the translator still hardcodes
/// `KeyCode::Up + KeyModifiers::ALT` (`tui/keymap.rs`), so this hint cannot yet
/// read a user override. The exact accessor needed is
/// `KeybindingsManager::get_keys("app.message.dequeue")` reached from a
/// `KeybindingsManager` owned by the shell, with `KeybindingsManager::matches`
/// replacing that hardcoded arm. Until both exist, the hint reports the chord
/// the binary provably consumes: the compiled default on every platform whose
/// default set is not Windows-flavoured, and the translator's chord on Windows
/// where the Windows-flavoured default set declares `alt+q` instead.
const QUEUED_EDIT_TRANSLATOR_CHORD: &str = "alt+up";

/// Resolve the key id for the platform named in `host` (`std::env::consts::OS`
/// vocabulary).
pub(super) fn queued_edit_key_id_for(host: &str) -> String {
    let platform = match host {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    keybindings::default_definitions(platform, false)
        .into_iter()
        .find(|definition| definition.id == QUEUED_EDIT_BINDING)
        .map(|definition| definition.default_keys)
        .unwrap_or_default()
        .into_iter()
        .find(|key| key.ends_with("up") || key == "up")
        .unwrap_or_else(|| QUEUED_EDIT_TRANSLATOR_CHORD.to_owned())
}

/// The resolved key id for this process, computed once.
fn queued_edit_key_id() -> &'static str {
    static RESOLVED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| queued_edit_key_id_for(std::env::consts::OS))
}

/// Human-readable label for one normalized key id.
///
/// `keymap::encode` is a terminal-*byte* encoder, not a label formatter, so the
/// display names live here. `macos` is a parameter (never a `cfg!` branch
/// inside) so both namings stay assertable on one host, matching the crate's
/// convention for platform-dependent text.
fn key_display_label(key_id: &str, macos: bool) -> String {
    let mut parts: Vec<&str> = key_id.split('+').collect();
    let base = parts.pop().unwrap_or_default().to_ascii_lowercase();
    let base = match base.as_str() {
        "up" | "arrowup" => "↑".to_owned(),
        "down" | "arrowdown" => "↓".to_owned(),
        "left" | "arrowleft" => "←".to_owned(),
        "right" | "arrowright" => "→".to_owned(),
        other => other.to_owned(),
    };
    let modifiers: Vec<String> = parts
        .iter()
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "alt" if macos => "option".to_owned(),
            "super" | "cmd" | "meta" if macos => "cmd".to_owned(),
            "control" => "ctrl".to_owned(),
            other => other.to_owned(),
        })
        .collect();
    if modifiers.is_empty() {
        base
    } else {
        format!("{}+{}", modifiers.join("+"), base)
    }
}

/// The `(option+↑ to edit)` / `(alt+↑ to edit)` affordance shown beside a
/// queued follow-up heading, matching the established `(ctrl+o to expand)`
/// style. `macos` is a parameter so both namings are assertable on one host.
pub(super) fn queued_edit_hint(key_id: &str, macos: bool) -> String {
    format!("({} to edit)", key_display_label(key_id, macos))
}

pub(super) fn render_pending_steering(
    state: &ShellState,
    width: u16,
    max_rows: usize,
) -> Vec<String> {
    if (state.steering_queue.is_empty() && state.follow_up_queue.is_empty()) || max_rows == 0 {
        return Vec::new();
    }
    let max_rows = max_rows.min(crate::tui::layout::MAX_STEERING_PREVIEW_ROWS);
    let count = state.steering_queue.len() + state.follow_up_queue.len();
    let label = if state.follow_up_queue.is_empty() {
        "Steering"
    } else if state.steering_queue.is_empty() {
        "Follow-up"
    } else {
        "Input"
    };
    let heading = if count == 1 {
        format!("{label}{}queued", semantic_separator(&state.theme))
    } else {
        format!(
            "{label}{}{} queued",
            semantic_separator(&state.theme),
            count
        )
    };
    // Receipt state is authoritative even before the delivery event arrives.
    // Sticky `/answer` and steering already claimed for persistence cannot edit.
    let editable = !state.follow_up_queue.is_empty()
        || state.steering_queue.iter().any(|entry| {
            entry.recall.as_ref().is_some_and(|receipt| receipt.is_pending())
        });
    let hint = if !editable {
        String::new()
    } else {
        format!(
            " {}",
            subdued_text(
                &state.theme,
                &queued_edit_hint(queued_edit_key_id(), cfg!(target_os = "macos"))
            )
        )
    };
    let mut lines = vec![fit_line(
        &format!(
            "{ACTIVITY_DETAIL_INDENT}{}{hint}",
            state
                .theme
                .bold(&state.theme.model_fg(state.model_lab, &heading))
        ),
        width,
    )];
    if max_rows == 1 {
        return lines;
    }

    let elbow = activity_elbow(&state.theme);
    let plain_prefix = format!("{ACTIVITY_DETAIL_INDENT}{elbow} ");
    let prefix = format!(
        "{ACTIVITY_DETAIL_INDENT}{} ",
        state.theme.model_fg(state.model_lab, elbow)
    );
    let hidden = count.saturating_sub(1);
    let hidden_suffix = if hidden == 0 {
        String::new()
    } else {
        format!("{}+{hidden} more", semantic_separator(&state.theme))
    };
    let available = usize::from(width)
        .saturating_sub(visible_width(&plain_prefix))
        .max(1);
    let preview_budget = available.saturating_sub(visible_width(&hidden_suffix));
    let display = state
        .steering_queue
        .first()
        .map(|entry| entry.display.as_str())
        .unwrap_or_else(|| state.follow_up_queue[0].composed.transcript_text.as_str());
    let preview = steering_preview_text(state, display);
    let preview = if visible_width(&preview) > preview_budget {
        clipped_steering_content(state, &preview, preview_budget)
    } else {
        preview
    };
    lines.push(fit_line(
        &format!(
            "{prefix}{}{}",
            state.theme.fg("muted", &preview),
            state.theme.fg("muted", &hidden_suffix),
        ),
        width,
    ));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_edit_binding_resolves_from_the_keybinding_registry() {
        // The compiled default declares this chord on every platform whose
        // default set is not Windows-flavoured; the hint must read the
        // registry rather than a literal copy of it.
        for host in ["macos", "linux", "freebsd"] {
            assert_eq!(
                queued_edit_key_id_for(host),
                "alt+up",
                "{host} must declare the registry binding for {QUEUED_EDIT_BINDING}"
            );
        }
        // Windows-flavoured defaults declare `alt+q` for the same binding while
        // the translator still consumes `KeyCode::Up + ALT`, so the hint names
        // the chord the binary accepts. Recorded in the TODO above.
        assert_eq!(
            queued_edit_key_id_for("windows"),
            QUEUED_EDIT_TRANSLATOR_CHORD
        );
        assert_eq!(
            keybindings::default_definitions("win32", false)
                .into_iter()
                .find(|definition| definition.id == QUEUED_EDIT_BINDING)
                .map(|definition| definition.default_keys),
            Some(vec!["alt+q".to_owned()]),
            "the recorded Windows divergence must stay real"
        );
    }

    #[test]
    fn queued_edit_hint_names_option_on_macos_and_alt_elsewhere() {
        assert_eq!(queued_edit_hint("alt+up", true), "(option+↑ to edit)");
        assert_eq!(queued_edit_hint("alt+up", false), "(alt+↑ to edit)");
        // Modifier and key display mapping is total, so an unexpected binding
        // never renders as an empty affordance.
        assert_eq!(key_display_label("ctrl+alt+down", true), "ctrl+option+↓");
        assert_eq!(key_display_label("super+up", false), "super+↑");
        assert_eq!(key_display_label("alt+q", true), "option+q");
    }
}

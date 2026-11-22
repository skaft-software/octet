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
    let key = key
        .split('/')
        .map(|key| crate::tui::keymap::keybindings::display_key_id(key, cfg!(target_os = "macos")))
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "{} {}",
        state.theme.bold(&state.theme.fg("model_accent", &key)),
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

fn truncate_plain_cell(value: &str, width: usize, ellipsis: &str) -> String {
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
    let (navigation_key, select_key) = ("Up/Down", "Enter");
    let separator = suggestion_separator(state);
    let mut segments = [
        // Scope is visually first but is the least useful segment at compact
        // widths, so its lower rank drops before the optional action tail.
        FooterSegment::optional(state.theme.fg("muted", &scope), 0),
        FooterSegment::primary(suggestion_key_hint(state, navigation_key, "navigate")),
        FooterSegment::optional(suggestion_key_hint(state, select_key, "select"), 2),
        FooterSegment::optional(suggestion_key_hint(state, "Esc", "close"), 1),
    ];
    fit_prioritized_footer("  ", &separator, &mut segments, width)
}

/// Paint the input suggestion rows with a theme's `suggestions_bg` fill,
/// padded to the reading column like the composer. Themes without the token
/// keep plain rows.
fn paint_suggestion_rows(
    state: &ShellState,
    mut lines: Vec<String>,
    width: u16,
    inset: usize,
) -> Vec<String> {
    let Some(background) = state.theme.role_rgb("suggestions_bg") else {
        return lines;
    };
    let layout = crate::tui::layout::PresentationLayout::new(&state.theme, width);
    // The painted region matches the shaded composer's fill span: the whole
    // terminal width when it bleeds, otherwise the reading column.
    let bleeds = layout.capped && state.theme.resolve::<bool>("surface_bleed").unwrap_or(true);
    let fill_width = if bleeds {
        usize::from(width)
    } else {
        usize::from(layout.content_width)
    }
    .min(usize::from(width));
    let split = if bleeds { 0 } else { inset };
    let mut painted = Vec::with_capacity(lines.len() + 1);
    // One blank filled row above the first item, so the box is not glued to
    // the word — the same cushion the shaded composer gives its draft.
    let blank_prefix = " ".repeat(split);
    painted.push(format!(
        "{blank_prefix}{}",
        state
            .theme
            .paint_row_background(background, &" ".repeat(fill_width))
    ));
    for row in &mut lines {
        let prefix: String = row.chars().take(split).collect();
        let content: String = row.chars().skip(split).collect();
        let content = fit_line(&content, fill_width as u16);
        let padding = fill_width.saturating_sub(visible_width(&content));
        let padded = format!("{content}{}", " ".repeat(padding));
        painted.push(format!(
            "{prefix}{}",
            state.theme.paint_row_background(background, &padded)
        ));
    }
    painted
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
    // The command marker and label continue the composer marker and draft
    // text columns, including the shaded/framed chrome's inner cushion.
    let inner_inset: u16 = match state.theme.resolve::<String>("composer").as_deref() {
        Some("shaded") => 1,
        Some("framed") => 2,
        _ => 0,
    };
    let popup_width = layout.content_width.saturating_sub(inner_inset * 2).max(1);
    let popup_prefix = " ".repeat(usize::from(layout.inset + inner_inset));

    // Keep one compact hint row below the choices when the theme shows it.
    // The footer makes autocomplete read as an inline continuation of the
    // composer.
    let show_footer = state
        .theme
        .resolve::<bool>("panel_footer_hints")
        .unwrap_or(true);
    // A filled popup spends one row on its top cushion.
    let fill_pad = usize::from(state.theme.role_rgb("suggestions_bg").is_some());
    let item_rows = max_rows
        .saturating_sub(usize::from(show_footer) + fill_pad)
        .max(1);
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
        let label = truncate_plain_cell(&display_label, label_width, state.theme.glyph("ellipsis"));
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
        let description = truncate_plain_cell(
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
    // A `panel_footer_hints = false` theme drops the compact hint row too.
    if show_footer {
        let footer =
            slash_suggestion_footer(state, popup_width, start, end, suggestions.len(), item_rows);
        lines.push(fit_line(&format!("{popup_prefix}{footer}"), width));
    }
    paint_suggestion_rows(state, lines, width, usize::from(layout.inset))
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
    let layout = crate::tui::layout::PresentationLayout::new(&state.theme, width);
    // The list opens level with the composer's input column, not the
    // terminal edge.
    let inner_inset: u16 = match state.theme.resolve::<String>("composer").as_deref() {
        Some("shaded") => 1,
        Some("framed") => 2,
        _ => 0,
    };
    let popup_width = layout.content_width.saturating_sub(inner_inset * 2).max(1);
    let popup_prefix = " ".repeat(usize::from(layout.inset + inner_inset));
    let matches = input_path_suggestions(state);
    if matches.is_empty() {
        // A scan in progress is reported rather than drawn as an empty popup:
        // the walk takes as long as the workspace takes, and silence here reads
        // as "`@` does not work" rather than "still reading the tree".
        if state.file_index_scanning
            && composer::active_mention(state.editor.text())
                .is_some_and(|query| !composer::is_path_query(query))
        {
            let marker = state.theme.glyph("prompt");
            let label = state
                .theme
                .fg("muted", &format!("{marker} scanning project files..."));
            return vec![fit_line(&format!("{popup_prefix}{label}"), width)];
        }
        return Vec::new();
    }
    let heading_label = if composer::active_mention(state.editor.text())
        .is_some_and(|query| !composer::is_path_query(query))
    {
        "project files"
    } else {
        "paths"
    };

    // A filled popup spends one row on its top cushion.
    let fill_pad = usize::from(state.theme.role_rgb("suggestions_bg").is_some());
    let item_rows = max_rows.saturating_sub(1 + fill_pad).min(5);
    let selected = state.path_selection.min(matches.len() - 1);
    let start = selected.saturating_sub(item_rows - 1);
    let marker = state.theme.glyph("prompt");
    let marker_width = visible_width(marker).max(1);
    let available_width = usize::from(popup_width)
        .saturating_sub(2 + marker_width + 1)
        .max(1);
    let mut lines = Vec::with_capacity(item_rows.saturating_add(1));
    for (index, suggestion) in matches.into_iter().enumerate().skip(start).take(item_rows) {
        let safe_path =
            sanitize_ordinary_surface_cell(&suggestion.completion, state.theme.unicode());
        let path = truncate_plain_cell(&safe_path, available_width, state.theme.glyph("ellipsis"));
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
        lines.push(fit_line(
            &format!("{popup_prefix}{}", fit_line(&choice, popup_width)),
            width,
        ));
    }

    let separator = suggestion_separator(state);
    let mut segments = [
        FooterSegment::optional(state.theme.fg("muted", heading_label), 0),
        FooterSegment::primary(suggestion_key_hint(state, "Tab", "complete")),
        FooterSegment::optional(suggestion_key_hint(state, "Up/Down", "navigate"), 1),
    ];
    lines.push(fit_prioritized_footer(
        &popup_prefix,
        &separator,
        &mut segments,
        width,
    ));
    paint_suggestion_rows(state, lines, width, usize::from(layout.inset))
}

/// Whether an extension autocomplete overlay is what the popup row shows.
pub(super) fn extension_autocomplete_shown(state: &ShellState) -> bool {
    !render_extension_autocomplete(state, 2, 2).is_empty()
}

/// The draft Tab produces for the slash popup, or `None` when Tab does nothing.
/// The one selector for the product path: the highlighted row completes, except
/// that a name typed in full stays itself while the selection is still at its
/// default (`/skill:review` must not become `/skill:review-pr`). Pi applies the
/// selected item the same way, with the exact name ranked first
/// (pi-tui dist/components/editor.js:540, dist/autocomplete.js:208-231). With
/// the popup dismissed only a unique prefix completes, and a popup that is not
/// on screen, or one an extension overlay replaced, completes nothing.
pub(super) fn slash_completion(
    state: &ShellState,
    popup_visible: bool,
    overlay_shown: bool,
) -> Option<String> {
    if overlay_shown || state.editor.cursor() != state.editor.text().len() {
        return None;
    }
    let suggestions = input_slash_suggestions(state);
    if state.slash_popup_dismissed {
        if suggestions.len() != 1 {
            return None;
        }
    } else if !popup_visible {
        return None;
    }
    let typed = state.editor.text().strip_prefix('/').unwrap_or_default();
    let selected = state
        .slash_selection
        .min(suggestions.len().saturating_sub(1));
    let exact = suggestions.iter().position(|entry| entry.name == typed);
    let chosen = match exact {
        Some(exact) if selected == 0 => exact,
        _ => selected,
    };
    let suggestion = suggestions.get(chosen)?;
    Some(format!(
        "/{}{}",
        suggestion.name,
        if suggestion.accepts_argument { " " } else { "" }
    ))
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
            suggestion_key_hint(state, "Tab", "accept")
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
    if !slash.is_empty() {
        return slash;
    }
    // An extension editor owns its own non-command completions (mentions, paths
    // and extension providers). The host popup is the command registry; it must
    // not open a second list beside the component's own.
    if state
        .extension_ui
        .remote
        .mount(octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement::Editor)
        .is_some()
    {
        return Vec::new();
    }
    render_path_suggestions(state, width, max_rows)
}

pub(super) enum PendingPreview {
    Steering(String),
    Wrapped(String),
}

pub(super) fn pending_previews(state: &ShellState, width: usize) -> Vec<PendingPreview> {
    let mixed = !state.steering_queue.is_empty() && !state.follow_up_queue.is_empty();
    let mut previews = Vec::new();
    if !state.pending_controls.is_empty() {
        previews.push(PendingPreview::Wrapped(super::sanitize_for_terminal(
            &state.pending_controls.join("; "),
        )));
    }
    let ellipsis = state.theme.glyph("ellipsis");
    for entry in state.steering_queue.iter() {
        let (first, omitted) = entry
            .display
            .split_once('\n')
            .map_or((entry.display.as_str(), false), |(first, _)| {
                (first.trim_end_matches('\r'), true)
            });
        let mut text = if mixed {
            String::from("Steering: ")
        } else {
            String::new()
        };
        text.push_str(&super::sanitize_for_terminal(first));
        if omitted {
            text.push_str(ellipsis);
        }
        previews.push(PendingPreview::Steering(truncate_plain_cell(
            &text, width, ellipsis,
        )));
    }
    for entry in state.follow_up_queue.iter() {
        let text = super::sanitize_for_terminal(&entry.composed.transcript_text);
        previews.push(PendingPreview::Wrapped(if mixed {
            format!("Follow-up: {text}")
        } else {
            text
        }));
    }
    previews
}

/// Namespaced binding the compiled default declares for restoring a queued
/// message (`crates/octet-coding-agent/src/tui/keymap/keybindings.rs`).
const QUEUED_EDIT_BINDING: &str = "app.message.dequeue";

/// Shown only if the registry ever declares no key for the binding.
const QUEUED_EDIT_FALLBACK_CHORD: &str = "alt+up";

/// The chord a key set restores queued messages with: its first default for
/// the binding. Windows and WSL use the Windows-flavoured set, which binds
/// `alt+q` because Windows Terminal claims Alt+arrows for panes, so the hint
/// must not name `alt+up` there.
fn queued_edit_key_id_in(bindings: &keybindings::KeybindingsManager) -> String {
    bindings
        .get_keys(QUEUED_EDIT_BINDING)
        .first()
        .cloned()
        .unwrap_or_else(|| QUEUED_EDIT_FALLBACK_CHORD.to_owned())
}

/// The resolved key id for this process, computed once from the same key set
/// the input translator matches, including WSL detection.
fn queued_edit_key_id() -> &'static str {
    static RESOLVED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RESOLVED
        .get_or_init(|| queued_edit_key_id_in(&keybindings::KeybindingsManager::current_platform()))
}

/// The `(Option+Up to edit)` / `(Alt+Up to edit)` affordance shown beside a
/// queued follow-up heading. `macos` keeps both platform namings testable.
pub(super) fn queued_edit_hint(key_id: &str, macos: bool) -> String {
    format!("({} to edit)", keybindings::display_key_id(key_id, macos))
}

pub(super) fn render_pending_steering(
    state: &ShellState,
    width: u16,
    max_rows: usize,
) -> Vec<String> {
    if (state.steering_queue.is_empty()
        && state.follow_up_queue.is_empty()
        && state.pending_controls.is_empty())
        || max_rows == 0
    {
        return Vec::new();
    }
    let count =
        state.steering_queue.len() + state.follow_up_queue.len() + state.pending_controls.len();
    let label = if !state.pending_controls.is_empty() {
        "Controls"
    } else if state.follow_up_queue.is_empty() {
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
            entry
                .recall
                .as_ref()
                .is_some_and(|receipt| receipt.is_pending())
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
    let prefix = format!(
        "{ACTIVITY_DETAIL_INDENT}{} ",
        state.theme.model_fg(state.model_lab, elbow)
    );
    let continuation = format!("{ACTIVITY_DETAIL_INDENT}  ");
    let mut content = Vec::new();
    let available = usize::from(width).saturating_sub(visible_width(&prefix));
    for preview in pending_previews(state, available) {
        match preview {
            PendingPreview::Steering(text) => content.push(fit_line(
                &format!("{prefix}{}", state.theme.fg("muted", &text)),
                width,
            )),
            PendingPreview::Wrapped(text) => content.extend(super::wrap_hanging(
                &state.theme.fg("muted", &text),
                &prefix,
                &continuation,
                width,
            )),
        }
    }
    let budget = max_rows - 1;
    if content.len() > budget {
        let hidden = content.len() - budget.saturating_sub(1);
        content.truncate(budget.saturating_sub(1));
        let ellipsis = if state.theme.unicode() { "…" } else { "..." };
        content.push(fit_line(
            &format!(
                "{continuation}{}",
                subdued_text(
                    &state.theme,
                    &format!(
                        "{ellipsis} {hidden} queued {} hidden",
                        super::plural(hidden, "row", "rows")
                    )
                )
            ),
            width,
        ));
    }
    lines.extend(content);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::{test_theme_source_with, TerminalBackground};

    #[test]
    fn cards_slash_menu_marker_and_command_align_with_shaded_composer() {
        let mut state = ShellState {
            theme: test_theme_source_with(
                include_str!("../../../../../examples/themes/Cards.toml"),
                TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
                TerminalBackground::Dark,
            ),
            ..Default::default()
        };
        state.editor.set_text("/");
        for width in [40, 80, 120] {
            let composer = crate::tui::composer_surface::render_composer_surface(
                &state,
                width,
                std::time::Instant::now(),
            );
            let composer = composer
                .iter()
                .map(|row| strip_terminal_sequences(row))
                .find(|row| row.contains("❯ /"))
                .expect("composer prompt with slash draft");
            let choices = render_slash_suggestions(&state, width, 6);
            let selected = strip_terminal_sequences(&choices[0]);
            let marker_column = composer.find('❯').unwrap();
            let command_column = composer.find('/').unwrap();
            assert_eq!(
                selected.find('❯'),
                Some(marker_column),
                "{width}: {selected:?}"
            );
            assert_eq!(
                selected.find('/'),
                Some(command_column),
                "{width}: {selected:?}"
            );
            assert!(choices
                .iter()
                .all(|row| visible_width(row) <= width as usize));
        }
    }

    #[test]
    fn queued_edit_binding_resolves_from_the_keybinding_registry() {
        // The hint names the chord the translator's key set binds: Alt+Up
        // generally, Alt+Q in the Windows-flavoured set used on Windows and WSL.
        for (platform, wsl, expected) in [
            ("darwin", false, "alt+up"),
            ("linux", false, "alt+up"),
            ("freebsd", false, "alt+up"),
            ("win32", false, "alt+q"),
            ("linux", true, "alt+q"),
        ] {
            let bindings = keybindings::KeybindingsManager::with_platform(
                platform,
                wsl,
                std::collections::BTreeMap::new(),
            );
            assert_eq!(
                queued_edit_key_id_in(&bindings),
                expected,
                "{platform} (wsl: {wsl})"
            );
        }
        assert_eq!(queued_edit_hint("alt+q", false), "(Alt+Q to edit)");
    }

    #[test]
    fn queued_edit_hint_names_option_on_macos_and_alt_elsewhere() {
        assert_eq!(queued_edit_hint("alt+up", true), "(Option+Up to edit)");
        assert_eq!(queued_edit_hint("alt+up", false), "(Alt+Up to edit)");
        assert_eq!(
            queued_edit_hint("ctrl+alt+down", true),
            "(Ctrl+Option+Down to edit)"
        );
        assert_eq!(queued_edit_hint("super+up", false), "(Super+Up to edit)");
        assert_eq!(queued_edit_hint("alt+q", true), "(Option+Q to edit)");
    }
}

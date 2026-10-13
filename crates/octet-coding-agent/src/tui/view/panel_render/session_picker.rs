//! The session and message pickers: row ordering, row rendering, and the
//! chrome (title, scope, filter line, hints, footer, delete prompt) above them.
//!
//! Split out of `panel_render.rs` because these two pickers are the only panel
//! surfaces that list `SessionMeta` rather than a homogeneous list of strings.
//! They therefore own their own text shaping, relative-age formatting, id
//! compaction and home-path shortening instead of bending the generic
//! select-list renderer, whose row model is deliberately label-plus-description
//! and knows nothing about workspaces or unreadable sessions.

use super::search_filter::match_session_search;
use super::*;
use crate::tui::view::semantic_separator;

/// Return the displayed session indices after named filtering and sorting.
pub(in crate::tui::view) fn session_picker_ordering(picker: &PickerState) -> Vec<usize> {
    let rows = picker.active_rows();
    let query = parse_search_query(&picker.filter);
    let mut scored = rows
        .iter()
        .enumerate()
        .filter_map(|(index, meta)| {
            if picker.named_only
                && meta
                    .name
                    .as_deref()
                    .is_none_or(|name| name.trim().is_empty())
            {
                return None;
            }
            let score = if let Some((searched, hits)) = &picker.entry_search {
                if searched == &picker.filter {
                    let text = hits.get(&meta.path)?;
                    text.to_lowercase()
                        .find(&picker.filter.to_lowercase())
                        .unwrap_or(0) as f64
                } else {
                    match_session_search(meta, &query)?
                }
            } else {
                match_session_search(meta, &query)?
            };
            Some((index, score))
        })
        .collect::<Vec<_>>();

    match picker.sort {
        // Store discovery is already newest-first. Keeping this order makes a
        // filtered recent view stable and avoids a second filesystem sort.
        PickerSort::Recent => {}
        PickerSort::Relevance => scored.sort_by(|(left, a), (right, b)| {
            a.partial_cmp(b)
                .unwrap_or(Ordering::Equal)
                .then_with(|| rows[*right].modified.cmp(&rows[*left].modified))
                .then_with(|| rows[*left].id.cmp(&rows[*right].id))
        }),
        PickerSort::Threaded => {
            // Iterative traversal, preserving discovery order among siblings.
            // Workspace-local ids are never linked across store directories;
            // corrupt cycles and filtered-out parents cannot hide a session.
            let by_id = scored
                .iter()
                .map(|(index, _)| {
                    (
                        (rows[*index].path.parent(), rows[*index].id.as_str()),
                        *index,
                    )
                })
                .collect::<std::collections::HashMap<_, _>>();
            let mut children: std::collections::HashMap<usize, Vec<usize>> =
                std::collections::HashMap::new();
            let mut roots = Vec::new();
            for (index, _) in &scored {
                let parent = rows[*index]
                    .forked_from_session_id
                    .as_deref()
                    .and_then(|id| by_id.get(&(rows[*index].path.parent(), id)).copied());
                if let Some(parent) = parent.filter(|parent| parent != index) {
                    children.entry(parent).or_default().push(*index);
                } else {
                    roots.push(*index);
                }
            }
            let mut seen = std::collections::HashSet::new();
            let mut ordered = Vec::new();
            for root in roots
                .into_iter()
                .chain(scored.iter().map(|(index, _)| *index))
            {
                let mut pending = vec![root];
                while let Some(index) = pending.pop() {
                    if !seen.insert(index) {
                        continue;
                    }
                    ordered.push((index, 0.0));
                    if let Some(children) = children.get(&index) {
                        pending.extend(children.iter().rev().copied());
                    }
                }
            }
            scored = ordered;
        }
        PickerSort::Name => scored.sort_by(|(left, left_score), (right, right_score)| {
            let left_meta = &rows[*left];
            let right_meta = &rows[*right];
            left_meta
                .title
                .to_ascii_lowercase()
                .cmp(&right_meta.title.to_ascii_lowercase())
                .then_with(|| right_meta.modified.cmp(&left_meta.modified))
                .then_with(|| left_meta.id.cmp(&right_meta.id))
                .then_with(|| {
                    left_score
                        .partial_cmp(right_score)
                        .unwrap_or(Ordering::Equal)
                })
        }),
        PickerSort::Messages => scored.sort_by(|(left, left_score), (right, right_score)| {
            let left_meta = &rows[*left];
            let right_meta = &rows[*right];
            right_meta
                .message_count
                .cmp(&left_meta.message_count)
                .then_with(|| {
                    left_score
                        .partial_cmp(right_score)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| right_meta.modified.cmp(&left_meta.modified))
                .then_with(|| left_meta.id.cmp(&right_meta.id))
        }),
    }
    scored.into_iter().map(|(index, _)| index).collect()
}

fn shorten_home_path(path: &std::path::Path) -> String {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return path.display().to_string();
    };
    if path == home {
        return "~".to_owned();
    }
    path.strip_prefix(&home).map_or_else(
        |_| path.display().to_string(),
        |relative| {
            if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            }
        },
    )
}

pub(in crate::tui::view) fn format_age(modified: SystemTime, now: SystemTime) -> String {
    let seconds = now
        .duration_since(modified)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    if seconds < 60 {
        return "now".to_owned();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    if days < 365 {
        return format!("{}mo", days / 30);
    }
    format!("{}y", days / 365)
}

fn picker_header_title(picker: &PickerState) -> String {
    let scope = match picker.scope {
        PickerScope::Current => "Current Folder",
        PickerScope::All => "All",
    };
    format!("{} ({scope})", picker.surface.title)
}

fn picker_scope_text(theme: &OctetTheme, picker: &PickerState) -> String {
    let (current, all) = if theme.unicode() {
        ("◉ Current Folder", "○ All")
    } else {
        ("[*] Current Folder", "[ ] All")
    };
    let current = if picker.scope == PickerScope::Current {
        theme.fg("model_accent", current)
    } else {
        subdued_text(theme, current)
    };
    let all = if picker.scope == PickerScope::All {
        theme.fg("model_accent", all)
    } else {
        subdued_text(theme, all)
    };
    format!(
        "{current} | {all}  {}  Sort: {}",
        if picker.named_only {
            "Name: Named"
        } else {
            "Name: All"
        },
        picker.sort.label()
    )
}

fn picker_header_line(state: &ShellState, picker: &PickerState, width: u16) -> String {
    let terminal_width = width;
    let plan = PresentationLayout::new(&state.theme, width);
    let inset = usize::from(plan.inset);
    let width = usize::from(plan.content_width);
    let left = state.theme.bold(&panel_cell(
        &picker_header_title(picker),
        state.theme.unicode(),
    ));
    let right = picker_scope_text(&state.theme, picker);
    let right = sexy_tui_rs::truncate_to_width(&right, width, Some(state.theme.glyph("ellipsis")));
    let gap = width
        .saturating_sub(visible_width(&left))
        .saturating_sub(visible_width(&right))
        .max(1);
    fit_line(
        &format!(
            "{}{left}{}{}{}",
            " ".repeat(inset),
            " ".repeat(gap),
            right,
            " ".repeat(inset)
        ),
        terminal_width,
    )
}

fn picker_filter_line(state: &ShellState, picker: &PickerState, width: u16) -> String {
    let (label, value) = match picker.rename.as_deref() {
        Some(value) => ("Rename", value),
        None => ("Filter", picker.filter.as_str()),
    };
    let terminal_width = width;
    let plan = PresentationLayout::new(&state.theme, width);
    let prefix = format!(
        "{}{}  ",
        " ".repeat(usize::from(plan.inset)),
        subdued_text(&state.theme, label)
    );
    let available =
        usize::from(plan.inset + plan.content_width).saturating_sub(visible_width(&prefix));
    let ellipsis = state.theme.glyph("ellipsis");
    let value = panel_cell(value, state.theme.unicode());
    if value.is_empty() {
        let placeholder = if label == "Rename" {
            "enter a session name"
        } else {
            "type to filter"
        };
        let placeholder = sexy_tui_rs::truncate_to_width(placeholder, available, Some(ellipsis));
        fit_line(
            &format!(
                "{prefix}{CURSOR_MARKER}{}",
                subdued_text(&state.theme, &placeholder)
            ),
            terminal_width,
        )
    } else {
        let query = sexy_tui_rs::truncate_to_width(&value, available, Some(ellipsis));
        fit_line(
            &format!(
                "{prefix}{}{CURSOR_MARKER}",
                state.theme.fg("foreground", &query)
            ),
            terminal_width,
        )
    }
}

fn panel_action_hint(state: &ShellState, key: &str, verb: &str) -> String {
    format!(
        "{} {}",
        state.theme.bold(&state.theme.fg("model_accent", key)),
        state.theme.fg("muted", verb)
    )
}

/// Render complete priority-ordered action hints through the one shared footer
/// primitive. Scope-like context drops before the rightmost optional actions.
pub(in crate::tui::view) fn panel_action_footer(
    state: &ShellState,
    width: u16,
    prefix: &str,
    scope: Option<&str>,
    primary: (&str, &str),
    optional: &[(&str, &str)],
) -> String {
    let mut segments = Vec::with_capacity(optional.len() + 2);
    if let Some(scope) = scope.filter(|scope| !scope.is_empty()) {
        segments.push(FooterSegment::optional(state.theme.fg("muted", scope), 0));
    }
    segments.push(FooterSegment::primary(panel_action_hint(
        state, primary.0, primary.1,
    )));
    for (index, (key, verb)) in optional.iter().enumerate() {
        // The visual right edge is the first optional action to disappear.
        let drop_rank = u8::try_from(optional.len().saturating_sub(index)).unwrap_or(u8::MAX);
        segments.push(FooterSegment::optional(
            panel_action_hint(state, key, verb),
            drop_rank,
        ));
    }
    let separator = state.theme.fg("muted", semantic_separator(&state.theme));
    fit_prioritized_footer(prefix, &separator, &mut segments, width)
}

fn picker_shows_advanced_filter_hints(picker: &PickerState) -> bool {
    // Progressive disclosure: `re:` / `"phrase"` / transcript search are
    // powerful but noisy as always-on chrome. Show them once the filter
    // actually uses them, keeping the default picker to scope + transcripts.
    picker.entry_search.is_some() || picker.filter.contains("re:") || picker.filter.contains('"')
}

fn picker_hints(state: &ShellState, picker: &PickerState, width: u16) -> (String, String) {
    let now = Instant::now();
    let inset = " ".repeat(usize::from(
        PresentationLayout::new(&state.theme, width).inset,
    ));
    let first = if picker.confirming_delete {
        panel_action_footer(
            state,
            width,
            &inset,
            Some(&picker_delete_prompt(state, picker)),
            ("enter", "confirm"),
            &[("esc", "cancel")],
        )
    } else if picker.rename.is_some() {
        panel_action_footer(
            state,
            width,
            &inset,
            None,
            ("enter", "save"),
            &[("esc", "cancel")],
        )
    } else if matches!(
        picker.surface.lifecycle,
        OrdinarySurfaceLifecycle::Success(_)
            | OrdinarySurfaceLifecycle::RecoverableError(_)
            | OrdinarySurfaceLifecycle::Cancelled(_)
    ) {
        render_ordinary_status(&state.theme, &picker.surface.lifecycle, now).map_or_else(
            || {
                if picker_shows_advanced_filter_hints(picker) {
                    panel_action_footer(
                        state,
                        width,
                        &inset,
                        None,
                        ("tab", "scope"),
                        &[
                            ("^f", "transcripts"),
                            ("re:<pattern>", "filter"),
                            ("\"phrase\"", "exact"),
                        ],
                    )
                } else {
                    panel_action_footer(
                        state,
                        width,
                        &inset,
                        None,
                        ("tab", "scope"),
                        &[("^f", "transcripts")],
                    )
                }
            },
            |status| fit_line(&format!("{inset}{status}"), width),
        )
    } else if picker_shows_advanced_filter_hints(picker) {
        panel_action_footer(
            state,
            width,
            &inset,
            None,
            ("tab", "scope"),
            &[
                ("^f", "transcripts"),
                ("re:<pattern>", "filter"),
                ("\"phrase\"", "exact"),
            ],
        )
    } else {
        panel_action_footer(
            state,
            width,
            &inset,
            None,
            ("tab", "scope"),
            &[("^f", "transcripts")],
        )
    };
    let second = if picker.confirming_delete || picker.rename.is_some() {
        String::new()
    } else {
        panel_action_footer(
            state,
            width,
            &inset,
            None,
            ("^s", "sort"),
            &[
                ("^n", "named"),
                ("^x", "trash"),
                ("^p", "path on/off"),
                ("^r", "rename"),
            ],
        )
    };
    (first, second)
}

/// The trash confirmation, naming the session it acts on.
fn picker_delete_prompt(state: &ShellState, picker: &PickerState) -> String {
    let unicode = state.theme.unicode();
    let Some(meta) = session_picker_ordering(picker)
        .get(picker.selected)
        .and_then(|index| picker.active_rows().get(*index))
    else {
        return "Move session to trash?".to_owned();
    };
    let named = meta.name.as_deref().unwrap_or(&meta.title).trim();
    let label = if named.is_empty() || is_unreadable_session(meta) {
        compact_session_id(&meta.id, unicode)
    } else {
        sexy_tui_rs::truncate_to_width(
            &panel_cell(named, unicode),
            40,
            Some(state.theme.glyph("ellipsis")),
        )
    };
    let (open, close) = if unicode {
        ("\u{201c}", "\u{201d}")
    } else {
        ("\"", "\"")
    };
    format!("Move {open}{label}{close} to trash?")
}

fn picker_workspace(meta: &crate::session_store::SessionMeta) -> String {
    meta.workspace.as_deref().map_or_else(
        || {
            meta.path
                .parent()
                .and_then(|path| path.file_name())
                .map_or_else(
                    || "(unknown workspace)".to_owned(),
                    |name| name.to_string_lossy().into(),
                )
        },
        shorten_home_path,
    )
}

fn is_unreadable_session(meta: &crate::session_store::SessionMeta) -> bool {
    meta.title.eq_ignore_ascii_case("(unreadable session)")
}

fn compact_session_id(id: &str, unicode: bool) -> String {
    let characters = id.chars().collect::<Vec<_>>();
    if characters.len() <= 16 {
        return id.to_owned();
    }
    let prefix = characters[..8].iter().collect::<String>();
    let suffix = characters[characters.len().saturating_sub(6)..]
        .iter()
        .collect::<String>();
    let ellipsis = if unicode { "…" } else { "..." };
    format!("{prefix}{ellipsis}{suffix}")
}

fn session_title(
    state: &ShellState,
    meta: &crate::session_store::SessionMeta,
    is_current: bool,
) -> String {
    let mut label = String::new();
    if meta.pinned {
        label.push_str(state.theme.glyph("bullet"));
        label.push(' ');
    }
    if is_unreadable_session(meta) {
        let compact_id = compact_session_id(&meta.id, state.theme.unicode());
        label.push('(');
        label.push_str(&join_ordinary_metadata(
            &state.theme,
            &["unreadable session", &compact_id],
        ));
        label.push(')');
    } else {
        label.push_str(&meta.title);
    }
    if meta.forked_from_session_id.is_some() {
        label.push_str(" (fork)");
    }
    if is_current {
        label.push_str(" (current)");
    }
    panel_cell(&label, state.theme.unicode())
}

fn session_detail(
    state: &ShellState,
    meta: &crate::session_store::SessionMeta,
    picker: &PickerState,
    now: SystemTime,
) -> String {
    let mut details = Vec::with_capacity(5);
    if picker.scope == PickerScope::All {
        details.push(picker_workspace(meta));
    }
    details.push(format_age(meta.modified, now));
    if meta.message_count > 0 {
        let suffix = if meta.message_count == 1 {
            "msg"
        } else {
            "msgs"
        };
        details.push(format!("{} {suffix}", meta.message_count));
    }
    if is_unreadable_session(meta) {
        details.push("transcript unavailable".to_owned());
    }
    if picker.show_path {
        details.push(shorten_home_path(&meta.path));
    }
    let sanitized = details
        .iter()
        .map(|detail| panel_cell(detail, state.theme.unicode()))
        .collect::<Vec<_>>();
    let parts = sanitized.iter().map(String::as_str).collect::<Vec<_>>();
    join_ordinary_metadata(&state.theme, &parts)
}

pub(super) fn session_rows_are_stacked(state: &ShellState, width: u16, body_rows: usize) -> bool {
    body_rows >= 2 && PresentationLayout::new(&state.theme, width).picker == PickerLayout::Stacked
}

fn render_picker_row(
    state: &ShellState,
    meta: &crate::session_store::SessionMeta,
    picker: &PickerState,
    selected: bool,
    confirming: bool,
    stacked: bool,
    width: u16,
) -> Vec<String> {
    let is_current = picker.current_session_path.as_ref() == Some(&meta.path);
    let label = session_title(state, meta, is_current);
    let now = SystemTime::now();
    let right = session_detail(state, meta, picker, now);
    let inset = " ".repeat(usize::from(
        PresentationLayout::new(&state.theme, width).inset,
    ));
    let cursor = if selected {
        format!(
            "{inset}{} ",
            state.theme.fg("model_accent", state.theme.glyph("prompt"))
        )
    } else {
        format!("{inset}  ")
    };
    let ellipsis = state.theme.glyph("ellipsis");
    let label = if confirming {
        state.theme.fg("error", &label)
    } else if is_current {
        state.theme.fg("model_accent", &label)
    } else if meta.name.is_some() {
        state.theme.fg("warning", &label)
    } else {
        label
    };
    let label = if selected {
        state.theme.bold(&label)
    } else {
        label
    };

    if stacked {
        let available = usize::from(width).saturating_sub(visible_width(&cursor));
        let label = sexy_tui_rs::truncate_to_width(&label, available.max(1), Some(ellipsis));
        let detail_prefix = format!("{inset}  ");
        let detail_width = usize::from(width).saturating_sub(visible_width(&detail_prefix));
        let detail = sexy_tui_rs::truncate_to_width(&right, detail_width, Some(ellipsis));
        return vec![
            fit_line(&format!("{cursor}{label}"), width),
            fit_line(
                &format!("{detail_prefix}{}", subdued_text(&state.theme, &detail)),
                width,
            ),
        ];
    }

    let right_width = visible_width(&right);
    let available = usize::from(width)
        .saturating_sub(visible_width(&cursor))
        .saturating_sub(right_width)
        .saturating_sub(1);
    let label = sexy_tui_rs::truncate_to_width(&label, available.max(1), Some(ellipsis));
    let spacing = usize::from(width)
        .saturating_sub(visible_width(&cursor))
        .saturating_sub(visible_width(&label))
        .saturating_sub(right_width)
        .max(1);
    vec![fit_line(
        &format!(
            "{cursor}{label}{}{}",
            " ".repeat(spacing),
            subdued_text(&state.theme, &right)
        ),
        width,
    )]
}

fn tree_prefix(index: usize, total: usize, unicode: bool) -> &'static str {
    if total <= 1 {
        ""
    } else if unicode {
        if index + 1 == total {
            "└─ "
        } else {
            "├─ "
        }
    } else if index + 1 == total {
        "`- "
    } else {
        "|- "
    }
}

fn render_message_item(
    state: &ShellState,
    message: &ForkMessage,
    index: usize,
    total: usize,
    selected: bool,
    width: u16,
) -> Vec<String> {
    let display = if message.whole_conversation {
        "Whole conversation".to_owned()
    } else {
        message.text.replace(['\n', '\r'], " ")
    };
    let display = panel_cell(&display, state.theme.unicode());
    let prefix = tree_prefix(index, total, state.theme.unicode());
    let cursor = if selected {
        format!(
            "{} ",
            state.theme.fg("model_accent", state.theme.glyph("prompt"))
        )
    } else {
        "  ".to_owned()
    };
    let available = usize::from(width)
        .saturating_sub(visible_width(&cursor))
        .saturating_sub(visible_width(prefix));
    let display = sexy_tui_rs::truncate_to_width(
        display.trim(),
        available.max(1),
        Some(state.theme.glyph("ellipsis")),
    );
    let display = if selected {
        state.theme.bold(&display)
    } else {
        display
    };
    vec![
        fit_line(
            &format!("{cursor}{}{display}", subdued_text(&state.theme, prefix)),
            width,
        ),
        fit_line(
            &subdued_text(
                &state.theme,
                &format!("  Message {} of {}", index + 1, total),
            ),
            width,
        ),
        String::new(),
    ]
}

fn session_empty_message(picker: &PickerState) -> String {
    if picker.named_only {
        match picker.scope {
            PickerScope::Current => {
                "named sessions in the current workspace. Press ^n to show all, or tab to view all."
                    .to_owned()
            }
            PickerScope::All => "named sessions. Press ^n to show all.".to_owned(),
        }
    } else {
        match picker.scope {
            PickerScope::Current => {
                "sessions in the current workspace. Press tab to view all.".to_owned()
            }
            PickerScope::All => "sessions".to_owned(),
        }
    }
}

pub(super) fn render_session_picker(
    state: &ShellState,
    picker: &PickerState,
    width: u16,
    max_rows: usize,
    rule: &str,
) -> Vec<String> {
    let show_borders = state.theme.layout_for_width(width).show_panel_borders && max_rows >= 6;
    let inset = " ".repeat(usize::from(
        PresentationLayout::new(&state.theme, width).inset,
    ));
    let mut lines = Vec::with_capacity(max_rows);
    if show_borders {
        lines.push(subdued_text(&state.theme, rule));
    }
    lines.push(picker_header_line(state, picker, width));
    if max_rows >= 4 {
        if let Some(purpose) = picker.surface.purpose.as_deref() {
            lines.push(fit_line(
                &format!(
                    "{inset}{}",
                    subdued_text(&state.theme, &panel_cell(purpose, state.theme.unicode()))
                ),
                width,
            ));
        }
    }
    if max_rows >= 2 {
        lines.push(picker_filter_line(state, picker, width));
    }
    // Keep one body row for the selected session or explicit lifecycle state.
    // At short heights, footer hints yield before that semantic content.
    let remaining_border_rows = usize::from(show_borders);
    let reserve_body_row = usize::from(max_rows > lines.len() + remaining_border_rows);
    let hint_rows = max_rows.saturating_sub(lines.len() + remaining_border_rows + reserve_body_row);
    if hint_rows > 0 {
        let (first, second) = picker_hints(state, picker, width);
        lines.push(first);
        if hint_rows > 1 {
            lines.push(second);
        }
    }

    let body_rows = max_rows.saturating_sub(lines.len() + remaining_border_rows);
    if body_rows > 0 {
        let ordering = session_picker_ordering(picker);
        if matches!(
            &picker.surface.lifecycle,
            OrdinarySurfaceLifecycle::Loading(_) | OrdinarySurfaceLifecycle::Empty(_)
        ) {
            if let Some(status) =
                render_ordinary_status(&state.theme, &picker.surface.lifecycle, Instant::now())
            {
                lines.push(fit_line(&format!("{inset}{status}"), width));
            }
        } else if ordering.is_empty() {
            let lifecycle = OrdinarySurfaceLifecycle::empty(session_empty_message(picker));
            if let Some(status) = render_ordinary_status(&state.theme, &lifecycle, Instant::now()) {
                lines.push(fit_line(&format!("{inset}{status}"), width));
            }
        } else {
            // Wide pickers use a title line plus a dim metadata line. This
            // keeps timestamps/counts from competing with a long prompt and
            // gives the title the full terminal width before truncation.
            let stacked = session_rows_are_stacked(state, width, body_rows);
            let row_height = usize::from(stacked) + 1;
            let show_indicator = ordering.len() > body_rows / row_height
                && body_rows >= row_height.saturating_add(1);
            let visible_rows = body_rows.saturating_sub(usize::from(show_indicator));
            let visible_items = (visible_rows / row_height).min(ordering.len());
            let window = panel_window(picker.selected, ordering.len(), visible_items);
            for position in window {
                if let Some(meta) = picker.active_rows().get(ordering[position]) {
                    lines.extend(render_picker_row(
                        state,
                        meta,
                        picker,
                        position == picker.selected,
                        picker.confirming_delete && position == picker.selected,
                        stacked,
                        width,
                    ));
                }
            }
            if show_indicator {
                let selected = picker.selected.min(ordering.len().saturating_sub(1));
                lines.push(fit_line(
                    &subdued_text(
                        &state.theme,
                        &format!("{inset}({}/{})", selected + 1, ordering.len()),
                    ),
                    width,
                ));
            }
        }
    }
    if show_borders {
        lines.push(subdued_text(&state.theme, rule));
    }
    lines.truncate(max_rows);
    lines
}

pub(super) fn render_message_picker(
    state: &ShellState,
    picker: &MessagePicker,
    width: u16,
    max_rows: usize,
    rule: &str,
) -> Vec<String> {
    let show_borders = state.theme.layout_for_width(width).show_panel_borders && max_rows >= 5;
    let border_rows = usize::from(show_borders) * 2;
    let mut lines = Vec::with_capacity(max_rows);
    if show_borders {
        lines.push(subdued_text(&state.theme, rule));
    }
    lines.push(fit_line(
        &state
            .theme
            .bold(&panel_cell(&picker.surface.title, state.theme.unicode())),
        width,
    ));
    if let Some(purpose) = picker.surface.purpose.as_deref() {
        lines.push(fit_line(
            &subdued_text(&state.theme, &panel_cell(purpose, state.theme.unicode())),
            width,
        ));
    }
    if !matches!(
        &picker.surface.lifecycle,
        OrdinarySurfaceLifecycle::Ready | OrdinarySurfaceLifecycle::Empty(_)
    ) {
        if let Some(status) =
            render_ordinary_status(&state.theme, &picker.surface.lifecycle, Instant::now())
        {
            lines.push(fit_line(&status, width));
        }
    }
    // Keep one body row for the focused message or explicit empty state. At a
    // constrained height the action footer yields rather than hiding status.
    let can_show_footer = max_rows >= lines.len().saturating_add(2 + border_rows);
    if can_show_footer {
        let navigation = if state.theme.unicode() {
            "↑↓"
        } else {
            "up/down"
        };
        lines.push(panel_action_footer(
            state,
            width,
            "",
            None,
            (navigation, "select"),
            &[("enter", "fork"), ("esc", "cancel")],
        ));
    }
    let body_rows = max_rows.saturating_sub(lines.len() + border_rows);
    if body_rows > 0 {
        if picker.messages.is_empty() {
            let lifecycle = if matches!(
                &picker.surface.lifecycle,
                OrdinarySurfaceLifecycle::Empty(_)
            ) {
                picker.surface.lifecycle.clone()
            } else {
                OrdinarySurfaceLifecycle::empty("user messages")
            };
            if let Some(status) = render_ordinary_status(&state.theme, &lifecycle, Instant::now()) {
                lines.push(fit_line(&format!("  {status}"), width));
            }
        } else {
            let total = picker.messages.len();
            let show_indicator = total.saturating_mul(3) > body_rows;
            let item_rows = body_rows.saturating_sub(usize::from(show_indicator));
            let visible = item_rows / 3;
            let window = panel_window(picker.selected, total, visible.min(total));
            for index in window {
                if let Some(message) = picker.messages.get(index) {
                    lines.extend(render_message_item(
                        state,
                        message,
                        index,
                        total,
                        index == picker.selected,
                        width,
                    ));
                }
            }
            if show_indicator {
                let selected = picker.selected.min(total.saturating_sub(1));
                lines.push(fit_line(
                    &subdued_text(&state.theme, &format!("  ({}/{})", selected + 1, total)),
                    width,
                ));
            }
        }
    }
    if show_borders {
        lines.push(subdued_text(&state.theme, rule));
    }
    lines.truncate(max_rows);
    lines
}

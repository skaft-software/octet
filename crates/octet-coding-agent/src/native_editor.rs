//! Synchronous component-editor state. The frontend lease owns this service;
//! JavaScript receives observations and callback intents, never a mutable model.

use std::collections::BTreeMap;

use octet_agent::extension_process::ExtensionRequestFailure;
use serde::Deserialize;
use serde_json::{json, Value};
use sexy_tui_rs::{TextEditAction as Action, TextEditor};
use unicode_segmentation::UnicodeSegmentation;

const TEXT_BYTES: usize = 256 * 1024;
const EDITORS: usize = 16;
type Refusal = (ExtensionRequestFailure, String);

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Create {
        padding_x: u16,
        autocomplete_max_visible: u16,
    },
    Dispose {},
    Bind {},
    Read {
        #[serde(default)]
        field: Option<ReadField>,
    },
    SetText {
        text: String,
    },
    Insert {
        text: String,
    },
    Input {
        data: String,
        action: Option<KeyAction>,
        #[serde(default)]
        menu_action: Option<KeyAction>,
        disable_submit: bool,
    },
    Render {
        width: u16,
        rows: u16,
    },
    Padding {
        value: u16,
    },
    AutocompleteMaxVisible {
        value: u16,
    },
    AddHistory {
        text: String,
    },
    ConfigureAutocomplete {
        trigger_characters: Vec<String>,
    },
    Query {
        force: bool,
    },
    Suggestions {
        query_id: u64,
        prefix: String,
        items: Vec<Suggestion>,
    },
    Selection {},
    CancelAutocomplete {},
    Complete {
        revision: u64,
        #[serde(default)]
        submit: bool,
        lines: Vec<String>,
        cursor_line: usize,
        cursor_col: usize,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReadField {
    Text,
    Expanded,
    Lines,
    Cursor,
    Padding,
    AutocompleteMaxVisible,
    Pastes,
    UndoHistory,
    Autocomplete,
}

/// Semantic key translation is an adapter hook, not editable state. Rust owns
/// history/submit/paste decisions and applies every mutation to TextEditor.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum KeyAction {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    WordLeft,
    WordRight,
    Backspace,
    Delete,
    Newline,
    Submit,
    Undo,
    Redo,
    DeleteWordBackward,
    DeleteWordForward,
    DeleteToLineStart,
    DeleteToLineEnd,
    Yank,
    YankPop,
    HistoryPrevious,
    HistoryNext,
    PageUp,
    PageDown,
    JumpForward,
    JumpBackward,
    Ignore,
    Tab,
    Cancel,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Suggestion {
    id: usize,
    value: String,
    label: String,
    #[serde(default)]
    description: Option<String>,
}
struct Menu {
    revision: u64,
    query_id: u64,
    items: Vec<Suggestion>,
    prefix: String,
    selected: usize,
    force: bool,
}
struct PendingQuery {
    id: u64,
    revision: u64,
    force: bool,
    apply_single: bool,
}

struct Editor {
    model: TextEditor,
    triggers: Vec<String>,
    menu: Option<Menu>,
    pending_query: Option<PendingQuery>,
    query_sequence: u64,
    padding: u16,
    autocomplete_max_visible: u16,
    width: usize,
    visible: usize,
    scroll: usize,
    preferred_column: Option<usize>,
    snapped_from: Option<usize>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    history_draft_cursor: usize,
    paste_buffer: Option<String>,
    jump: Option<bool>,
    pastes: BTreeMap<u64, String>,
    paste_counter: u64,
}

impl Editor {
    fn snapshot(&self) -> Value {
        let prefix = &self.model.text()[..self.model.cursor()];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let col = prefix.rsplit('\n').next().unwrap().encode_utf16().count();
        json!({"text": self.model.text(), "lines": self.model.text().split('\n').collect::<Vec<_>>(),
            "cursor": {"line":line,"col":col}, "revision":self.model.revision(),
            "padding_x":self.padding, "autocomplete_max_visible":self.autocomplete_max_visible,
            "expanded":self.expanded(self.model.text()), "pastes":self.pastes.iter().filter(|(id,_)|self.markers().iter().any(|(_,_,marker_id)|marker_id == *id)).map(|(id,text)|json!([id,text])).collect::<Vec<_>>(),
            "undo_history":self.model.history_depths()})
    }

    fn markers(&self) -> Vec<(usize, usize, u64)> {
        if self.pastes.is_empty() || !self.model.text().contains("[paste #") {
            return Vec::new();
        }
        // Adjacent combining/prepend scalars can join a marker's edge. Atom
        // projection/movement must consume whole graphemes, not panic on a
        // perfectly valid Unicode draft whose regex range ends inside one.
        let boundaries = self
            .model
            .text()
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain([self.model.text().len()])
            .collect::<Vec<_>>();
        paste_pattern()
            .captures_iter(self.model.text())
            .filter_map(|capture| {
                let id = capture[1].parse::<u64>().ok()?;
                self.pastes.contains_key(&id).then(|| {
                    let range = capture.get(0).unwrap();
                    let start = boundaries
                        [boundaries.partition_point(|offset| *offset <= range.start()) - 1];
                    let end =
                        boundaries[boundaries.partition_point(|offset| *offset < range.end())];
                    (start, end, id)
                })
            })
            .collect()
    }

    fn expanded(&self, text: &str) -> String {
        if self.pastes.is_empty() || !text.contains("[paste #") {
            return text.to_owned();
        }
        paste_pattern()
            .replace_all(text, |capture: &regex::Captures<'_>| {
                capture[1]
                    .parse::<u64>()
                    .ok()
                    .and_then(|id| self.pastes.get(&id))
                    .cloned()
                    .unwrap_or_else(|| capture[0].to_owned())
            })
            .into_owned()
    }

    fn expanded_len(&self, text: &str) -> usize {
        let mut bytes = text.len();
        for capture in paste_pattern().captures_iter(text) {
            if let Some(value) = capture[1]
                .parse::<u64>()
                .ok()
                .and_then(|id| self.pastes.get(&id))
            {
                bytes = bytes
                    .saturating_sub(capture[0].len())
                    .saturating_add(value.len());
            }
        }
        bytes
    }

    fn paste(&mut self, text: &str) -> Result<(), Refusal> {
        let decoded = control_paste_pattern().replace_all(text, |capture: &regex::Captures<'_>| {
            let code = capture[1].parse::<u32>().unwrap_or(0);
            match code {
                65..=90 => char::from_u32(code - 64).unwrap().to_string(),
                97..=122 => char::from_u32(code - 96).unwrap().to_string(),
                _ => capture[0].to_owned(),
            }
        });
        let mut text = normalize(&decoded)
            .chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect::<String>();
        if text.starts_with(['/', '~', '.'])
            && self.model.text()[..self.model.cursor()]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            text.insert(0, ' ');
        }
        if self.expanded_len(self.model.text()) + text.len() > TEXT_BYTES {
            return Err(bounds("expanded editor draft exceeds 256 KiB"));
        }
        let lines = text.split('\n').count();
        let chars = text.encode_utf16().count();
        if lines > 10 || chars > 1000 {
            if self.pastes.len() >= 64
                || self.pastes.values().map(String::len).sum::<usize>() + text.len()
                    > 4 * 1024 * 1024
            {
                return Err(bounds("native paste recovery ledger is full"));
            }
            let id = self.paste_counter + 1;
            let marker = if lines > 10 {
                format!("[paste #{id} +{lines} lines]")
            } else {
                format!("[paste #{id} {chars} chars]")
            };
            self.pastes.insert(id, text);
            if let Err(error) = self.insert(&marker) {
                self.pastes.remove(&id);
                return Err(error);
            }
            self.paste_counter = id;
            Ok(())
        } else {
            self.insert(&text)
        }
    }

    fn check_insert(&self, text: &str) -> Result<(), Refusal> {
        if self.model.text().len() + text.len() > TEXT_BYTES {
            return Err(bounds("editor draft exceeds 256 KiB"));
        }
        if !self.pastes.is_empty() {
            let mut candidate = self.model.text().to_owned();
            candidate.insert_str(self.model.cursor(), text);
            if self.expanded_len(&candidate) > TEXT_BYTES {
                return Err(bounds("expanded editor draft exceeds 256 KiB"));
            }
        }
        Ok(())
    }

    fn insert(&mut self, text: &str) -> Result<(), Refusal> {
        let text = normalize(text);
        check_text(&text)?;
        self.check_insert(&text)?;
        self.model.apply(Action::Paste(text), self.width);
        self.history_index = None;
        Ok(())
    }

    fn history(&mut self, previous: bool) -> Result<(), Refusal> {
        let index = match (previous, self.history_index) {
            (true, None) if !self.history.is_empty() => Some(0),
            (true, Some(index)) => Some((index + 1).min(self.history.len() - 1)),
            (false, Some(0)) => None,
            (false, Some(index)) => Some(index - 1),
            _ => return Ok(()),
        };
        let text = index.map_or(&self.history_draft, |index| &self.history[index]);
        if self.expanded_len(text) > TEXT_BYTES {
            return Err(bounds("expanded editor history exceeds 256 KiB"));
        }
        if previous && self.history_index.is_none() {
            self.history_draft = self.model.text().to_owned();
            self.history_draft_cursor = self.model.cursor();
        }
        let text = index.map_or(&self.history_draft, |index| &self.history[index]);
        let cursor = if previous {
            0
        } else if index.is_none() {
            self.history_draft_cursor
        } else {
            text.len()
        };
        if self.history_index.is_none() {
            self.model
                .edit_range(0..self.model.text().len(), text, cursor);
        } else {
            self.model
                .continue_range_edit(0..self.model.text().len(), text, cursor);
        }
        self.history_index = index;
        Ok(())
    }

    fn projection(&self) -> sexy_tui_rs::TextEditorProjection {
        let atoms = self
            .markers()
            .into_iter()
            .map(|(start, end, _)| start..end)
            .collect::<Vec<_>>();
        self.model
            .projection_with_atoms(self.width, &atoms)
            .expect("native marker ranges are disjoint grapheme spans")
    }

    fn vertical(&mut self, down: bool) {
        let projection = self.projection();
        let Some(mut row) = (if down {
            projection.cursor_row().checked_add(1)
        } else {
            projection.cursor_row().checked_sub(1)
        }) else {
            return;
        };
        let source_col = self
            .snapped_from
            .map_or(projection.cursor().column(), |offset| {
                TextEditor::projection_for_layout(self.model.text(), projection.layout(), offset)
                    .cursor()
                    .column()
            });
        let old_preferred = self.preferred_column;
        while row < projection.lines().len() {
            let candidate = projection
                .vertical_row_target(
                    self.model.text(),
                    row,
                    source_col,
                    &mut self.preferred_column,
                )
                .unwrap();
            if let Some((start, end, _)) = self
                .markers()
                .into_iter()
                .find(|(start, end, _)| candidate >= *start && candidate < *end)
            {
                if down && start < projection.lines()[row].start() {
                    row += 1;
                    while row < projection.lines().len() && projection.lines()[row].start() < end {
                        row += 1;
                    }
                    if row < projection.lines().len() {
                        self.preferred_column = old_preferred;
                        continue;
                    }
                }
                self.snapped_from = Some(candidate);
                self.model.set_cursor(start);
            } else {
                self.snapped_from = None;
                self.model.set_cursor(candidate);
            }
            break;
        }
    }

    fn input(
        &mut self,
        data: &str,
        action: Option<KeyAction>,
        disabled: bool,
    ) -> Result<Option<String>, Refusal> {
        if !matches!(
            action,
            Some(KeyAction::Up | KeyAction::Down | KeyAction::PageUp | KeyAction::PageDown)
        ) {
            self.preferred_column = None;
            self.snapped_from = None;
        }
        if data.len() > TEXT_BYTES + 12 {
            return Err(bounds("editor input exceeds paste bound"));
        }
        if let Some(start) = data.find("\x1b[200~") {
            self.paste_buffer = Some(String::new());
            return self.input(&data[start + 6..], None, disabled);
        }
        if let Some(buffer) = &mut self.paste_buffer {
            if buffer.len() + data.len() > TEXT_BYTES + 6 {
                return Err(bounds("editor paste exceeds 256 KiB"));
            }
            buffer.push_str(data);
            if let Some(end) = buffer.find("\x1b[201~") {
                let buffer = self.paste_buffer.take().unwrap();
                self.paste(&buffer[..end])?;
                return self.input(&buffer[end + 6..], None, disabled);
            }
            return Ok(None);
        }
        if let Some(forward) = self.jump.take() {
            if !matches!(
                action,
                Some(KeyAction::JumpForward | KeyAction::JumpBackward)
            ) {
                if let Some(c) = data.chars().next().filter(|c| !c.is_control()) {
                    self.model.apply(
                        if forward {
                            Action::JumpForward(c)
                        } else {
                            Action::JumpBackward(c)
                        },
                        self.width,
                    );
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }
        let edit = match action {
            Some(KeyAction::Submit) => {
                if disabled {
                    return Ok(None);
                }
                if self.model.text()[..self.model.cursor()].ends_with('\\') {
                    let cursor = self.model.cursor();
                    self.model.edit_range(cursor - 1..cursor, "\n", cursor);
                    return Ok(None);
                }
                let text = self.expanded(self.model.text()).trim().to_owned();
                self.model.clear();
                self.history_index = None;
                return Ok(Some(text));
            }
            Some(KeyAction::HistoryPrevious) => {
                self.history(true)?;
                return Ok(None);
            }
            Some(KeyAction::HistoryNext) => {
                self.history(false)?;
                return Ok(None);
            }
            Some(KeyAction::Up)
                if self.projection().cursor_row() == 0
                    && (self.model.is_empty()
                        || self.history_index.is_some()
                        || self.model.cursor() == 0) =>
            {
                self.history(true)?;
                return Ok(None);
            }
            Some(KeyAction::Down)
                if self.history_index.is_some() && {
                    let p = self.projection();
                    p.cursor_row() + 1 == p.lines().len()
                } =>
            {
                self.history(false)?;
                return Ok(None);
            }
            Some(KeyAction::Up) if self.projection().cursor_row() == 0 => {
                self.model.set_cursor(0);
                return Ok(None);
            }
            Some(KeyAction::Home | KeyAction::End) => {
                let cursor = self.model.cursor();
                let target = if matches!(action, Some(KeyAction::Home)) {
                    self.model.text()[..cursor].rfind('\n').map_or(0, |p| p + 1)
                } else {
                    self.model.text()[cursor..]
                        .find('\n')
                        .map_or(self.model.text().len(), |p| p + cursor)
                };
                self.model.set_cursor(target);
                return Ok(None);
            }
            Some(KeyAction::Up | KeyAction::Down) => {
                self.vertical(matches!(action, Some(KeyAction::Down)));
                return Ok(None);
            }
            Some(KeyAction::PageUp | KeyAction::PageDown) => {
                for _ in 0..self.visible.max(1) {
                    self.vertical(matches!(action, Some(KeyAction::PageDown)));
                }
                return Ok(None);
            }
            Some(KeyAction::JumpForward | KeyAction::JumpBackward) => {
                self.jump = Some(matches!(action, Some(KeyAction::JumpForward)));
                return Ok(None);
            }
            Some(KeyAction::Ignore | KeyAction::Tab | KeyAction::Cancel) => return Ok(None),
            Some(KeyAction::Left) => Action::Left,
            Some(KeyAction::Right) => Action::Right,
            Some(KeyAction::WordLeft) => Action::WordLeft,
            Some(KeyAction::WordRight) => Action::WordRight,
            Some(KeyAction::Backspace) => Action::Backspace,
            Some(KeyAction::Delete) => Action::Delete,
            Some(KeyAction::Newline) => Action::Newline,
            Some(KeyAction::Undo) => Action::Undo,
            Some(KeyAction::Redo) => Action::Redo,
            Some(KeyAction::DeleteWordBackward) => Action::DeleteWordBackward,
            Some(KeyAction::DeleteWordForward) => Action::DeleteWordForward,
            Some(KeyAction::DeleteToLineStart) => Action::DeleteToLineStart,
            Some(KeyAction::DeleteToLineEnd) => Action::DeleteToLineEnd,
            Some(KeyAction::Yank) => Action::Yank,
            Some(KeyAction::YankPop) => Action::YankPop,
            None => {
                if data.chars().any(char::is_control) {
                    return Ok(None);
                }
                check_text(data)?;
                self.check_insert(data)?;
                for c in data.chars() {
                    self.model.apply(Action::Char(c), self.width);
                }
                self.history_index = None;
                return Ok(None);
            }
        };
        let caret = self.model.cursor();
        if let Some((start, end, _)) = self.markers().into_iter().find(|(start, end, _)| match edit
        {
            Action::Backspace | Action::Left => caret > *start && caret <= *end,
            Action::Delete | Action::Right => caret >= *start && caret < *end,
            _ => false,
        }) {
            match edit {
                Action::Left => self.model.set_cursor(start),
                Action::Right => self.model.set_cursor(end),
                _ => {
                    let mut candidate = self.model.clone();
                    candidate.edit_range(start..end, "", start);
                    if self.expanded_len(candidate.text()) > TEXT_BYTES {
                        return Err(bounds("expanded editor draft exceeds 256 KiB"));
                    }
                    self.model = candidate;
                }
            }
            return Ok(None);
        }
        // Actions capable of growing the model are checked transactionally. A
        // refusal cannot discard a prior draft or consume its undo/kill state.
        if matches!(
            edit,
            Action::Newline | Action::Yank | Action::YankPop | Action::Undo | Action::Redo
        ) || (!self.pastes.is_empty()
            && matches!(
                edit,
                Action::Backspace
                    | Action::Delete
                    | Action::DeleteWordBackward
                    | Action::DeleteWordForward
                    | Action::DeleteToLineStart
                    | Action::DeleteToLineEnd
            ))
        {
            let history_action = matches!(edit, Action::Undo | Action::Redo);
            let mut candidate = self.model.clone();
            candidate.apply(edit, self.width);
            if candidate.text().len() > TEXT_BYTES
                || self.expanded_len(candidate.text()) > TEXT_BYTES
            {
                return Err(bounds("editor draft exceeds 256 KiB"));
            }
            self.model = candidate;
            if history_action {
                self.history_index = None;
            }
        } else {
            let word_forward = matches!(edit, Action::WordRight);
            let word_backward = matches!(edit, Action::WordLeft);
            let right = matches!(edit, Action::Right);
            let changed = self.model.apply(edit, self.width);
            if right && !changed {
                self.preferred_column = Some(self.projection().cursor().column());
                self.model.set_cursor(self.model.cursor());
            }
            if word_forward || word_backward {
                if let Some((start, end, _)) = self.markers().into_iter().find(|(start, end, _)| {
                    self.model.cursor() > *start && self.model.cursor() < *end
                }) {
                    self.model
                        .set_cursor(if word_forward { end } else { start });
                }
            }
        }
        Ok(None)
    }

    fn query(&mut self, explicit_tab: bool) -> Value {
        let prefix = &self.model.text()[..self.model.cursor()];
        let slash = prefix.trim_start().starts_with('/') && !prefix.contains('\n');
        let symbol = symbol_context(prefix, &self.triggers);
        let force = if explicit_tab {
            !(slash && !prefix.trim_start().contains(' '))
        } else {
            self.menu.as_ref().is_some_and(|menu| menu.force)
        };
        self.pending_query = None;
        if prefix.is_empty() && !explicit_tab {
            self.menu = None;
            return json!({"query":null});
        }
        if !explicit_tab && !slash && !symbol && self.menu.is_none() {
            return json!({"query":null});
        }
        self.query_sequence += 1;
        self.pending_query = Some(PendingQuery {
            id: self.query_sequence,
            revision: self.model.revision(),
            force,
            apply_single: force && explicit_tab,
        });
        let snapshot = self.snapshot();
        json!({"query":{"id":self.query_sequence,"revision":self.model.revision(),
            "lines":snapshot["lines"],"cursor":snapshot["cursor"],"force":force,
            "delay":if !explicit_tab && !force && symbol {20} else {0}}})
    }

    fn selection(&self) -> Value {
        self.menu.as_ref().filter(|menu|menu.revision == self.model.revision()).map_or(json!({"selection":null}), |menu| {
            let snapshot = self.snapshot();
            json!({"selection":{"id":menu.items[menu.selected].id,"query_id":menu.query_id,
                "prefix":menu.prefix,"revision":self.model.revision(),"lines":snapshot["lines"],"cursor":snapshot["cursor"]}})
        })
    }

    fn suggestions(
        &mut self,
        id: u64,
        prefix: String,
        items: Vec<Suggestion>,
    ) -> Result<Value, Refusal> {
        let Some(pending) = self
            .pending_query
            .as_ref()
            .filter(|q| q.id == id && q.revision == self.model.revision())
        else {
            return Ok(json!({"accepted":false}));
        };
        let force = pending.force;
        let apply_single = pending.apply_single;
        check_text(&prefix)?;
        if items.len() > 128
            || items
                .iter()
                .map(|item| {
                    item.value.len()
                        + item.label.len()
                        + item.description.as_ref().map_or(0, String::len)
                })
                .sum::<usize>()
                > 512 * 1024
        {
            return Err(bounds("editor suggestions exceed bounded profile"));
        }
        for (index, item) in items.iter().enumerate() {
            if item.id != index {
                return Err(invalid("editor suggestion callback handle"));
            }
            for text in [&item.value, &item.label]
                .into_iter()
                .chain(item.description.iter())
            {
                if text.len() > 4096 || text.chars().any(char::is_control) {
                    return Err(invalid(
                        "editor suggestion contains controls or is oversized",
                    ));
                }
            }
        }
        self.pending_query = None;
        if items.is_empty() {
            self.menu = None;
            return Ok(json!({"accepted":true,"apply":false}));
        }
        let selected = items
            .iter()
            .position(|item| item.value == prefix)
            .or_else(|| {
                items
                    .iter()
                    .position(|item| item.value.starts_with(&prefix))
            })
            .unwrap_or(0);
        let apply = apply_single && items.len() == 1;
        self.menu = Some(Menu {
            revision: self.model.revision(),
            query_id: id,
            items,
            prefix,
            selected,
            force,
        });
        Ok(json!({"accepted":true,"apply":apply}))
    }

    fn menu_input(&mut self, action: Option<KeyAction>, disabled: bool) -> Option<Value> {
        if matches!(action, Some(KeyAction::Cancel)) {
            self.menu = None;
            self.pending_query = None;
            self.jump = None;
            return Some(json!({"change":null,"submit":null,"revision":self.model.revision()}));
        }
        let menu = self
            .menu
            .as_mut()
            .filter(|menu| menu.revision == self.model.revision())?;
        match action {
            Some(KeyAction::Up) => {
                menu.selected = menu.selected.checked_sub(1).unwrap_or(menu.items.len() - 1)
            }
            Some(KeyAction::Down) => menu.selected = (menu.selected + 1) % menu.items.len(),
            Some(KeyAction::Submit | KeyAction::Tab) => {
                return Some(
                    json!({"apply_completion":true,"submit_after":!disabled && matches!(action,Some(KeyAction::Submit)) && menu.prefix.starts_with('/')}),
                )
            }
            _ => return None,
        }
        Some(
            json!({"menu_handled":true,"change":null,"submit":null,"revision":self.model.revision()}),
        )
    }

    fn render(&mut self, width: u16, rows: u16) -> Result<Value, Refusal> {
        if width == 0 || rows == 0 {
            return Err(invalid("editor geometry must be nonzero"));
        }
        let padding = usize::from(self.padding).min(usize::from(width.saturating_sub(1)) / 2);
        let content_width = usize::from(width) - 2 * padding;
        self.width = content_width
            .saturating_sub(usize::from(padding == 0))
            .max(1);
        let projection = self.projection();
        let limit =
            (usize::from(rows) * 3 / 10).clamp(5, if self.menu.is_some() { 233 } else { 254 });
        let cursor = projection.cursor_row();
        if cursor < self.scroll {
            self.scroll = cursor;
        }
        if cursor >= self.scroll + limit {
            self.scroll = cursor + 1 - limit;
        }
        self.scroll = self
            .scroll
            .min(projection.lines().len().saturating_sub(limit));
        let end = (self.scroll + limit).min(projection.lines().len());
        self.visible = end - self.scroll;
        let lines = (self.scroll..end)
            .map(|row| {
                let range = &projection.lines()[row];
                if row == cursor {
                    let offset = projection.cursor().offset();
                    let before = &self.model.text()[range.start()..offset];
                    let tail = &self.model.text()[offset..range.visible_end().max(offset)];
                    let grapheme = tail.graphemes(true).next().unwrap_or(" ");
                    let after = if tail.is_empty() {
                        ""
                    } else {
                        &tail[grapheme.len()..]
                    };
                    json!({"before":before,"cursor":grapheme,"after":after})
                } else {
                    json!({"text":projection.line(self.model.text(), row).unwrap()})
                }
            })
            .collect::<Vec<_>>();
        Ok(
            json!({"lines":lines,"padding":padding,"content_width":content_width,
            "hidden_above":self.scroll,"hidden_below":projection.lines().len()-end,
            "autocomplete":self.menu.as_ref().filter(|menu|menu.revision == self.model.revision()).map(|menu|json!({"items":menu.items,"selected":menu.selected,"max_visible":self.autocomplete_max_visible}))}),
        )
    }
}

/// Native identity captured with a registry query; caret is a UTF-8 byte offset
/// in the expanded composer text, never an extension-supplied coordinate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerEditorIdentity {
    pub(crate) editor_id: String,
    pub(crate) revision: u64,
    pub(crate) registry_context: bool,
}

pub(crate) struct ComposerEditorSnapshot {
    pub(crate) text: String,
    pub(crate) cursor: usize,
}

#[derive(Default)]
pub(crate) struct EditorService {
    prefix: String,
    next: u64,
    editors: BTreeMap<String, Editor>,
    primary: Option<String>,
}

impl EditorService {
    pub(crate) fn for_mount(prefix: String) -> Self {
        Self {
            prefix,
            ..Self::default()
        }
    }

    pub(crate) fn composer_identity(&self) -> Option<ComposerEditorIdentity> {
        let id = self.primary.as_ref()?;
        let editor = self.editors.get(id)?;
        Some(ComposerEditorIdentity {
            editor_id: id.clone(),
            revision: editor.model.revision(),
            registry_context: !symbol_context(
                &editor.model.text()[..editor.model.cursor()],
                &editor.triggers,
            ),
        })
    }

    pub(crate) fn composer_snapshot(&self) -> Option<ComposerEditorSnapshot> {
        let id = self.primary.as_ref()?;
        let editor = self.editors.get(id)?;
        let prefix = &editor.model.text()[..editor.model.cursor()];
        Some(ComposerEditorSnapshot {
            text: editor.expanded(editor.model.text()),
            cursor: editor.expanded(prefix).len(),
        })
    }

    /// Host decisions enter the same native model, preserving its undo/paste
    /// recovery. Unchanged expanded echoes (including admission refusals) are
    /// observations, not edits that relocate the caret or flatten paste atoms.
    pub(crate) fn replace_composer(
        &mut self,
        text: &str,
        cursor: usize,
    ) -> Result<Option<bool>, Refusal> {
        let Some(editor) = self
            .primary
            .as_ref()
            .and_then(|id| self.editors.get_mut(id))
        else {
            return Ok(None);
        };
        check_text(text)?;
        if editor.expanded(editor.model.text()) == text {
            return Ok(Some(false));
        }
        if editor.expanded_len(text) > TEXT_BYTES {
            return Err(bounds("expanded editor draft exceeds 256 KiB"));
        }
        editor
            .model
            .edit_range(0..editor.model.text().len(), text, cursor);
        editor.menu = None;
        editor.pending_query = None;
        editor.history_index = None;
        editor.preferred_column = None;
        editor.snapped_from = None;
        Ok(Some(true))
    }

    pub(crate) fn request(&mut self, id: Option<&str>, operation: Value) -> Result<Value, Refusal> {
        let operation: Operation = serde_json::from_value(operation)
            .map_err(|error| invalid(&format!("invalid editor operation: {error}")))?;
        if let Operation::Create {
            padding_x,
            autocomplete_max_visible,
        } = operation
        {
            if id.is_some() {
                return Err(invalid("create cannot reuse an editor handle"));
            }
            if self.editors.len() >= EDITORS {
                return Err(bounds("too many component editors"));
            }
            self.next += 1;
            let id = format!("{}.editor.{}", self.prefix, self.next);
            self.editors.insert(
                id.clone(),
                Editor {
                    model: TextEditor::new(),
                    triggers: Vec::new(),
                    menu: None,
                    pending_query: None,
                    query_sequence: 0,
                    padding: padding_x,
                    autocomplete_max_visible: autocomplete_max_visible.clamp(3, 20),
                    width: 80,
                    visible: 1,
                    scroll: 0,
                    preferred_column: None,
                    snapped_from: None,
                    history: Vec::new(),
                    history_index: None,
                    history_draft: String::new(),
                    history_draft_cursor: 0,
                    paste_buffer: None,
                    jump: None,
                    pastes: BTreeMap::new(),
                    paste_counter: 0,
                },
            );
            return Ok(json!({"editor_id":id}));
        }
        let id = id.ok_or_else(|| invalid("editor handle required"))?;
        if matches!(operation, Operation::Dispose {}) {
            self.editors
                .remove(id)
                .ok_or_else(|| invalid("editor handle retired"))?;
            if self.primary.as_deref() == Some(id) {
                self.primary = None;
            }
            return Ok(json!({}));
        }
        let editor = self
            .editors
            .get_mut(id)
            .ok_or_else(|| invalid("editor handle retired"))?;
        let before = editor.model.text_revision();
        let before_cursor = editor.model.cursor();
        let mut submitted = None;
        let completion = matches!(operation, Operation::Complete { .. });
        match operation {
            Operation::Bind {} => {
                self.primary = Some(id.to_owned());
                return Ok(json!({}));
            }
            Operation::Read { field } => {
                return Ok(match field {
                    None => editor.snapshot(),
                    Some(field) => {
                        let value = match field {
                            ReadField::Text => json!(editor.model.text()),
                            ReadField::Expanded => json!(editor.expanded(editor.model.text())),
                            ReadField::Lines => {
                                json!(editor.model.text().split('\n').collect::<Vec<_>>())
                            }
                            ReadField::Cursor => {
                                let prefix = &editor.model.text()[..editor.model.cursor()];
                                json!({"line":prefix.bytes().filter(|byte|*byte == b'\n').count(),"col":prefix.rsplit('\n').next().unwrap().encode_utf16().count()})
                            }
                            ReadField::Padding => json!(editor.padding),
                            ReadField::AutocompleteMaxVisible => {
                                json!(editor.autocomplete_max_visible)
                            }
                            ReadField::Pastes => json!(editor
                                .pastes
                                .iter()
                                .filter(|(id, _)| editor
                                    .markers()
                                    .iter()
                                    .any(|(_, _, marker_id)| marker_id == *id))
                                .map(|(id, text)| json!([id, text]))
                                .collect::<Vec<_>>()),
                            ReadField::UndoHistory => json!(editor.model.history_depths()),
                            ReadField::Autocomplete => json!(editor
                                .menu
                                .as_ref()
                                .is_some_and(|menu| menu.revision == editor.model.revision())),
                        };
                        json!({"value":value,"revision":editor.model.revision()})
                    }
                })
            }
            Operation::Render { width, rows } => return editor.render(width, rows),
            Operation::SetText { text } => {
                let text = normalize(&text);
                check_text(&text)?;
                if editor.expanded_len(&text) > TEXT_BYTES {
                    return Err(bounds("expanded editor draft exceeds 256 KiB"));
                }
                editor
                    .model
                    .edit_range(0..editor.model.text().len(), &text, text.len());
                editor.history_index = None;
                editor.scroll = 0;
                editor.preferred_column = None;
                editor.snapped_from = None;
                editor.menu = None;
                editor.pending_query = None;
            }
            Operation::Insert { text } => {
                editor.menu = None;
                editor.pending_query = None;
                editor.insert(&text)?;
            }
            Operation::Input {
                data,
                action,
                menu_action,
                disable_submit,
            } => {
                if let Some(result) = editor.menu_input(menu_action.or(action), disable_submit) {
                    return Ok(result);
                }
                if matches!(
                    action,
                    Some(KeyAction::Undo | KeyAction::Redo | KeyAction::Newline)
                ) || data.contains("\x1b[200~")
                {
                    editor.menu = None;
                }
                editor.pending_query = None;
                submitted = editor.input(&data, action, disable_submit)?;
            }
            Operation::Padding { value } => editor.padding = value,
            Operation::AutocompleteMaxVisible { value } => {
                editor.autocomplete_max_visible = value.clamp(3, 20)
            }
            Operation::AddHistory { text } => {
                let text = normalize(text.trim());
                check_text(&text)?;
                if editor.expanded_len(&text) > TEXT_BYTES {
                    return Err(bounds("expanded editor history exceeds 256 KiB"));
                }
                if !text.is_empty() && editor.history.first() != Some(&text) {
                    editor.history.insert(0, text);
                    while editor.history.len() > 100
                        || editor.history.iter().map(String::len).sum::<usize>() > TEXT_BYTES
                    {
                        editor.history.pop();
                    }
                }
                editor.history_index = None;
            }
            Operation::ConfigureAutocomplete { trigger_characters } => {
                if trigger_characters.len() > 32
                    || trigger_characters
                        .iter()
                        .any(|c| c.is_empty() || c.len() > 16 || c.chars().any(char::is_control))
                {
                    return Err(invalid("editor autocomplete trigger profile"));
                }
                editor.triggers = trigger_characters;
                editor.menu = None;
                editor.pending_query = None;
            }
            Operation::Query { force } => return Ok(editor.query(force)),
            Operation::Suggestions {
                query_id,
                prefix,
                items,
            } => return editor.suggestions(query_id, prefix, items),
            Operation::Selection {} => return Ok(editor.selection()),
            Operation::CancelAutocomplete {} => {
                editor.menu = None;
                editor.pending_query = None;
            }
            Operation::Complete {
                revision,
                submit,
                lines,
                cursor_line,
                cursor_col,
            } => {
                if revision != editor.model.revision() {
                    return Err(invalid("stale editor completion"));
                }
                if lines.iter().any(|line| line.contains(['\n', '\r', '\t'])) {
                    return Err(invalid("completion lines must be normalized logical lines"));
                }
                let line = lines
                    .get(cursor_line)
                    .ok_or_else(|| invalid("completion cursor line"))?;
                let col = utf16_offset(line, cursor_col)
                    .ok_or_else(|| invalid("completion cursor column"))?;
                let cursor = lines[..cursor_line]
                    .iter()
                    .map(|line| line.len() + 1)
                    .sum::<usize>()
                    + col;
                let text = lines.join("\n");
                check_text(&text)?;
                if cursor != text.len()
                    && !text
                        .grapheme_indices(true)
                        .any(|(offset, _)| offset == cursor)
                {
                    return Err(invalid("completion cursor splits a grapheme"));
                }
                if editor.expanded_len(&text) > TEXT_BYTES {
                    return Err(bounds("expanded editor draft exceeds 256 KiB"));
                }
                if !editor
                    .model
                    .edit_range(0..editor.model.text().len(), &text, cursor)
                {
                    return Err(invalid("completion edit range"));
                }
                editor.menu = None;
                editor.pending_query = None;
                if submit {
                    submitted = editor.input("", Some(KeyAction::Submit), false)?;
                }
            }
            Operation::Create { .. } | Operation::Dispose {} => {
                unreachable!("handled before handle lookup")
            }
        }
        Ok(
            json!({"change":(before != editor.model.text_revision()).then(|| editor.model.text()),
            "submit":submitted,"revision":editor.model.revision(),"menu_handled":completion,"cursor_changed":before_cursor != editor.model.cursor()}),
        )
    }
}

fn paste_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN
        .get_or_init(|| regex::Regex::new(r"\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]").unwrap())
}
fn control_paste_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new("\x1b\\[(\\d+);5u").unwrap())
}
fn autocomplete_separator(c: char) -> bool {
    c.is_whitespace()
        || "，．：；！？（）［］｛｝“”‘’…—。、「」『』《》【】〔〕〈〉〝〞〟〰〽・".contains(c)
}
fn symbol_context(prefix: &str, triggers: &[String]) -> bool {
    prefix.char_indices().any(|(offset, c)| {
        if !matches!(c, '@' | '#') && !triggers.iter().any(|t| t == &c.to_string()) {
            return false;
        }
        let before = prefix[..offset].trim_end_matches(['(', '[', '{', '<', '`']);
        if !before.is_empty()
            && !before
                .chars()
                .next_back()
                .is_some_and(autocomplete_separator)
        {
            return false;
        }
        let after = &prefix[offset + c.len_utf8()..];
        if c == '@' && after.starts_with('"') {
            !after[1..].contains('"')
        } else {
            !after.chars().any(autocomplete_separator)
        }
    })
}
fn normalize(text: &str) -> String {
    TextEditor::normalize_paste(text).replace('\t', "    ")
}
fn check_text(text: &str) -> Result<(), Refusal> {
    if text.len() > TEXT_BYTES {
        return Err(bounds("editor text exceeds 256 KiB"));
    }
    if text.chars().any(|c| c != '\n' && c.is_control()) {
        return Err(invalid("editor text contains terminal controls"));
    }
    Ok(())
}
fn utf16_offset(text: &str, column: usize) -> Option<usize> {
    let mut units = 0;
    for (offset, c) in text.char_indices() {
        if units == column {
            return Some(offset);
        }
        units += c.len_utf16();
        if units > column {
            return None;
        }
    }
    (units == column).then_some(text.len())
}
fn invalid(detail: &str) -> Refusal {
    (ExtensionRequestFailure::InvalidRequest, detail.into())
}
fn bounds(detail: &str) -> Refusal {
    (ExtensionRequestFailure::BoundsExceeded, detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn component_handles_and_provider_receipts_are_bounded_and_retired() {
        let mut service = EditorService::for_mount("lease".into());
        let create = || json!({"op":"create","padding_x":0,"autocomplete_max_visible":5});
        let id = service.request(None, create()).unwrap()["editor_id"]
            .as_str()
            .unwrap()
            .to_owned();
        for _ in 1..EDITORS {
            service.request(None, create()).unwrap();
        }
        assert_eq!(
            service.request(None, create()).unwrap_err().0,
            ExtensionRequestFailure::BoundsExceeded
        );
        service.request(Some(&id), json!({"op":"bind"})).unwrap();
        let mut call = |op| service.request(Some(&id), op);
        call(json!({"op":"set_text","text":"@α"})).unwrap();
        let query = call(json!({"op":"query","force":false})).unwrap()["query"]["id"].clone();
        call(json!({"op":"input","data":"","action":"left","disable_submit":false})).unwrap();
        assert_eq!(call(json!({"op":"suggestions","query_id":query,"prefix":"α","items":[{"id":0,"value":"old","label":"old"}]})).unwrap()["accepted"],false);
        assert_eq!(
            call(json!({"op":"read","field":"autocomplete"})).unwrap()["value"],
            false
        );
        let query = call(json!({"op":"query","force":false})).unwrap()["query"]["id"].clone();
        let snapshot = call(json!({"op":"read"})).unwrap();
        for items in [
            json!([{"id":0,"value":"x","label":"\u{1b}[31mbad"}]),
            json!([{"id":10,"value":"x","label":"x"}]),
            json!(vec![json!({"id":0,"value":"x","label":"x"}); 129]),
        ] {
            assert!(
                call(json!({"op":"suggestions","query_id":query,"prefix":"@","items":items}))
                    .is_err()
            );
            assert_eq!(call(json!({"op":"read"})).unwrap(), snapshot);
        }
        call(json!({"op":"suggestions","query_id":query,"prefix":"@","items":[{"id":0,"value":"new","label":"new"}]})).unwrap();
        call(json!({"op":"input","data":"","action":"right","disable_submit":false})).unwrap();
        assert_eq!(
            call(json!({"op":"selection"})).unwrap()["selection"],
            Value::Null,
            "an old menu cannot apply against a new caret"
        );
        call(json!({"op":"set_text","text":"👩‍💻"})).unwrap();
        let snapshot = call(json!({"op":"read"})).unwrap();
        let revision = snapshot["revision"].clone();
        for (lines, col) in [(json!(["👩‍💻"]), 1), (json!(["👩‍💻"]), 2), (json!(["x\ny"]), 1)]
        {
            assert!(call(json!({"op":"complete","revision":revision,"lines":lines,"cursor_line":0,"cursor_col":col})).is_err());
            assert_eq!(call(json!({"op":"read"})).unwrap(), snapshot);
        }
        call(json!({"op":"dispose"})).unwrap();
        assert!(call(json!({"op":"read"})).is_err());
        assert!(service.composer_identity().is_none());
        service.request(None, create()).unwrap();
    }

    #[test]
    fn paste_expansion_budget_is_transactional_and_host_echo_retains_native_caret() {
        let mut service = EditorService::for_mount("lease".into());
        let id = service
            .request(
                None,
                json!({"op":"create","padding_x":0,"autocomplete_max_visible":5}),
            )
            .unwrap()["editor_id"]
            .as_str()
            .unwrap()
            .to_owned();
        service.request(Some(&id), json!({"op":"bind"})).unwrap();
        service.request(Some(&id),json!({"op":"input","data":format!("\x1b[200~{}\x1b[201~","x".repeat(TEXT_BYTES)),"action":null,"disable_submit":false})).unwrap();
        let snapshot = service.request(Some(&id), json!({"op":"read"})).unwrap();
        let raw = snapshot["text"].as_str().unwrap();
        for op in [
            json!({"op":"insert","text":"x"}),
            json!({"op":"set_text","text":format!("{raw}{raw}")}),
        ] {
            assert_eq!(
                service.request(Some(&id), op).unwrap_err().0,
                ExtensionRequestFailure::BoundsExceeded
            );
            assert_eq!(
                service.request(Some(&id), json!({"op":"read"})).unwrap(),
                snapshot
            );
        }
        service
            .request(
                Some(&id),
                json!({"op":"input","data":"","action":"left","disable_submit":false}),
            )
            .unwrap();
        let before = service.request(Some(&id), json!({"op":"read"})).unwrap();
        assert_eq!(
            service
                .replace_composer(&"x".repeat(TEXT_BYTES), TEXT_BYTES)
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            service.request(Some(&id), json!({"op":"read"})).unwrap(),
            before
        );
        let duplicate = raw.repeat(2);
        assert_eq!(
            service
                .replace_composer(&duplicate, duplicate.len())
                .unwrap_err()
                .0,
            ExtensionRequestFailure::BoundsExceeded
        );
        assert_eq!(
            service.request(Some(&id), json!({"op":"read"})).unwrap(),
            before
        );
    }

    #[test]
    fn paste_atoms_accept_adjacent_combining_scalars_and_reject_expansion_fanout() {
        let mut service = EditorService::for_mount("lease".into());
        let id = service
            .request(
                None,
                json!({"op":"create","padding_x":0,"autocomplete_max_visible":5}),
            )
            .unwrap()["editor_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut call = |op| service.request(Some(&id), op);
        call(json!({"op":"input","data":format!("\x1b[200~{}\x1b[201~","x".repeat(140000)),"action":null,"disable_submit":false})).unwrap();
        let raw = call(json!({"op":"read","field":"text"})).unwrap()["value"]
            .as_str()
            .unwrap()
            .to_owned();
        call(json!({"op":"input","data":"\u{0301}","action":null,"disable_submit":false})).unwrap();
        call(json!({"op":"render","width":80,"rows":24})).unwrap();
        let saved = call(json!({"op":"read"})).unwrap();
        assert_eq!(
            call(json!({"op":"set_text","text":raw.repeat(1024)}))
                .unwrap_err()
                .0,
            ExtensionRequestFailure::BoundsExceeded
        );
        assert_eq!(call(json!({"op":"read"})).unwrap(), saved);
        call(json!({"op":"input","data":"","action":"backspace","disable_submit":false})).unwrap();
        assert_eq!(
            call(json!({"op":"read","field":"text"})).unwrap()["value"],
            ""
        );
        call(json!({"op":"input","data":"","action":"undo","disable_submit":false})).unwrap();
        assert_eq!(
            call(json!({"op":"read","field":"text"})).unwrap()["value"],
            saved["text"]
        );
        // A newly registered ID cannot activate a guessed, duplicate marker and
        // overrun the budget after the native text edit already committed.
        call(json!({"op":"set_text","text":"[paste #2 140000 chars]"})).unwrap();
        let before = call(json!({"op":"read"})).unwrap();
        assert_eq!(call(json!({"op":"input","data":format!("\x1b[200~{}\x1b[201~","y".repeat(140000)),"action":null,"disable_submit":false})).unwrap_err().0,ExtensionRequestFailure::BoundsExceeded);
        assert_eq!(call(json!({"op":"read"})).unwrap(), before);
    }

    #[test]
    fn text_cursor_undo_and_completion_are_native_and_bounded() {
        let mut service = EditorService::default();
        let id = service
            .request(
                None,
                json!({"op":"create","padding_x":0,"autocomplete_max_visible":5}),
            )
            .unwrap()["editor_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut call = |body| service.request(Some(&id), body);
        call(json!({"op":"set_text","text":"a👩‍💻z"})).unwrap();
        call(json!({"op":"input","data":"","action":"left","disable_submit":false})).unwrap();
        assert_eq!(
            call(json!({"op":"read"})).unwrap()["cursor"],
            json!({"line":0,"col":6})
        );
        call(json!({"op":"insert","text":"X"})).unwrap();
        call(json!({"op":"input","data":"","action":"undo","disable_submit":false})).unwrap();
        assert_eq!(call(json!({"op":"read"})).unwrap()["text"], "a👩‍💻z");
        let snapshot = call(json!({"op":"read"})).unwrap();
        assert!(call(json!({"op":"insert","text":"x".repeat(TEXT_BYTES)})).is_err());
        assert_eq!(call(json!({"op":"read"})).unwrap(), snapshot);
        assert!(call(
            json!({"op":"complete","revision":0,"lines":["wrong"],"cursor_line":0,"cursor_col":5})
        )
        .is_err());
        assert_eq!(call(json!({"op":"read"})).unwrap(), snapshot);
        call(json!({"op":"dispose"})).unwrap();
        assert!(call(json!({"op":"read"})).is_err());
    }
}

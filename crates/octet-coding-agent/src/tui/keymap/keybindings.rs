//! Namespaced, configurable keybindings with conflict reporting and platform
//! defaults.
//!
//! Ported from the upstream reference `packages/tui/src/keybindings.ts`
//! (registry, `defaultKeys`, `KeybindingsManager.rebuild`, `getConflicts`,
//! `getResolvedBindings`) and `packages/coding-agent/src/core/keybindings.ts`
//! (additive `KEYBINDINGS` overlay, `useWindowsKeybindings`, the win32 undo
//! default, legacy-name migration, and `keybindings.json` loading).
//!
//! The manager is a pure data model: it does not translate terminal bytes.
//! Callers compare a translated [`crossterm::event::KeyEvent`] against a
//! namespaced id with [`KeybindingsManager::matches`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

/// One keybinding definition: a namespaced id, its default keys, and a
/// human-readable description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybindingDefinition {
    /// Namespaced id, for example `tui.editor.undo`.
    pub id: String,
    /// Default key ids. An empty slice means "unbound by default".
    pub default_keys: Vec<String>,
    /// Human-readable description.
    pub description: String,
}

/// A key claimed by more than one user binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybindingConflict {
    /// The contested key id.
    pub key: String,
    /// The namespaced binding ids that claim `key`.
    pub keybindings: Vec<String>,
}

/// `true` when the platform uses the Windows-flavoured default set.
///
/// Upstream also treats a Linux host inside WSL as Windows-like.
#[must_use]
pub fn use_windows_keybindings(platform: &str, wsl: bool) -> bool {
    platform == "win32" || (platform == "linux" && wsl)
}

/// The additive `app.*` overlay plus the platform-tuned editor defaults.
///
/// The returned list is the full registry in definition order: every
/// `tui.*` definition followed by every `app.*` definition.
#[must_use]
pub fn default_definitions(platform: &str, wsl: bool) -> Vec<KeybindingDefinition> {
    let windows = use_windows_keybindings(platform, wsl);
    // Platform-specific tree bindings were withdrawn with `/tree`; this
    // helper still keys the remaining Windows-only defaults.
    let _darwin = platform == "darwin";

    BASE_DEFINITIONS
        .iter()
        .map(|(id, keys, description)| {
            let default_keys: Vec<String> = match *id {
                "tui.editor.undo" => vec![if platform == "win32" {
                    "ctrl+z".to_owned()
                } else if windows {
                    "alt+z".to_owned()
                } else {
                    "ctrl+-".to_owned()
                }],
                "tui.altScreen.previousPrompt" => {
                    if windows {
                        vec!["ctrl+up".to_owned()]
                    } else {
                        strings(keys)
                    }
                }
                "tui.altScreen.nextPrompt" => {
                    if windows {
                        vec!["ctrl+down".to_owned()]
                    } else {
                        strings(keys)
                    }
                }
                "tui.altScreen.search" => vec![if windows {
                    "ctrl+f".to_owned()
                } else {
                    "ctrl+shift+f".to_owned()
                }],
                "app.suspend" if platform == "win32" => Vec::new(),
                "app.model.cycleBackward" if windows => vec!["alt+p".to_owned()],
                "app.message.followUp" if windows => vec!["ctrl+q".to_owned()],
                "app.message.dequeue" if windows => vec!["alt+q".to_owned()],
                "app.clipboard.pasteImage" if windows => vec!["alt+v".to_owned()],
                _ => strings(keys),
            };
            KeybindingDefinition {
                id: (*id).to_owned(),
                default_keys,
                description: (*description).to_owned(),
            }
        })
        .collect()
}

fn strings(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|key| (*key).to_owned()).collect()
}

/// Normalize a single key id so modifier order and spelling do not matter.
#[must_use]
pub fn normalize_key_id(raw: &str) -> String {
    let raw = raw.trim();
    let (prefix, base) = if let Some(prefix) = raw.strip_suffix('+') {
        (prefix.strip_suffix('+').unwrap_or(prefix), "+")
    } else {
        raw.rsplit_once('+').unwrap_or(("", raw))
    };
    let parts: Vec<&str> = prefix.split('+').filter(|part| !part.is_empty()).collect();
    let base = match base.to_ascii_lowercase().as_str() {
        "esc" => "escape".to_owned(),
        "return" => "enter".to_owned(),
        other => other.to_owned(),
    };
    if base.is_empty() {
        return String::new();
    }
    let mut modifiers: Vec<String> = parts
        .iter()
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect();
    const ORDER: [&str; 4] = ["ctrl", "shift", "alt", "super"];
    modifiers.sort_by_key(|modifier| {
        ORDER
            .iter()
            .position(|candidate| candidate == modifier)
            .unwrap_or(usize::MAX)
    });
    modifiers.dedup();
    if modifiers.is_empty() {
        base
    } else {
        format!("{}+{base}", modifiers.join("+"))
    }
}

/// Canonical key id for a terminal event, in the [`normalize_key_id`] space.
#[must_use]
pub fn key_event_id(key: &KeyEvent) -> String {
    let base = match key.code {
        KeyCode::Char(' ') => "space".to_owned(),
        KeyCode::Char(character) if character.is_ascii_alphabetic() => {
            character.to_ascii_lowercase().to_string()
        }
        KeyCode::Char(character) => character.to_string(),
        KeyCode::Enter => "enter".to_owned(),
        KeyCode::Esc => "escape".to_owned(),
        KeyCode::Tab | KeyCode::BackTab => "tab".to_owned(),
        KeyCode::Backspace => "backspace".to_owned(),
        KeyCode::Delete => "delete".to_owned(),
        KeyCode::Insert => "insert".to_owned(),
        KeyCode::Home => "home".to_owned(),
        KeyCode::End => "end".to_owned(),
        KeyCode::PageUp => "pageup".to_owned(),
        KeyCode::PageDown => "pagedown".to_owned(),
        KeyCode::Up => "up".to_owned(),
        KeyCode::Down => "down".to_owned(),
        KeyCode::Left => "left".to_owned(),
        KeyCode::Right => "right".to_owned(),
        KeyCode::F(number) => format!("f{number}"),
        _ => return String::new(),
    };
    let mut modifiers = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        modifiers.push("ctrl");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        || key.code == KeyCode::BackTab
        || matches!(key.code, KeyCode::Char(c) if c.is_ascii_uppercase())
    {
        modifiers.push("shift");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        modifiers.push("alt");
    }
    if key.modifiers.contains(KeyModifiers::SUPER) {
        modifiers.push("super");
    }
    if modifiers.is_empty() {
        base
    } else {
        format!("{}+{base}", modifiers.join("+"))
    }
}

/// Resolve namespaced bindings against user overrides and report conflicts.
#[derive(Clone, Debug)]
pub struct KeybindingsManager {
    definitions: Vec<KeybindingDefinition>,
    user_bindings: BTreeMap<String, Vec<String>>,
    keys_by_id: BTreeMap<String, Vec<String>>,
    conflicts: Vec<KeybindingConflict>,
    config_path: Option<std::path::PathBuf>,
}

impl KeybindingsManager {
    /// Platform defaults without filesystem access (also used by deterministic tests).
    #[must_use]
    pub fn current_platform() -> Self {
        Self::with_platform(Self::platform(), Self::is_wsl(), BTreeMap::new())
    }

    fn platform() -> &'static str {
        if cfg!(windows) {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        }
    }

    fn is_wsl() -> bool {
        cfg!(target_os = "linux")
            && (std::env::var_os("WSL_DISTRO_NAME").is_some()
                || std::env::var_os("WSL_INTEROP").is_some())
    }

    /// Load the user's configuration; project files never change input ownership.
    #[must_use]
    pub fn for_user() -> Self {
        dirs::home_dir().map_or_else(Self::current_platform, |home| {
            Self::create(&home.join(".octet"), Self::platform(), Self::is_wsl())
        })
    }

    /// Build a manager from explicit definitions and user overrides.
    #[must_use]
    pub fn new(
        definitions: Vec<KeybindingDefinition>,
        user_bindings: BTreeMap<String, Vec<String>>,
    ) -> Self {
        let mut manager = Self {
            definitions,
            user_bindings,
            keys_by_id: BTreeMap::new(),
            conflicts: Vec::new(),
            config_path: None,
        };
        manager.rebuild();
        manager
    }

    /// Build a manager with platform-tuned defaults.
    #[must_use]
    pub fn with_platform(
        platform: &str,
        wsl: bool,
        user_bindings: BTreeMap<String, Vec<String>>,
    ) -> Self {
        Self::new(default_definitions(platform, wsl), user_bindings)
    }

    /// Load `<agent_dir>/keybindings.json` and build the manager with the
    /// legacy-name migration applied, remembering the path for [`Self::reload`].
    #[must_use]
    pub fn create(agent_dir: &Path, platform: &str, wsl: bool) -> Self {
        let path = agent_dir.join("keybindings.json");
        let mut manager = Self::with_platform(platform, wsl, Self::load_from_file(&path));
        manager.config_path = Some(path);
        manager
    }

    /// Re-read the remembered config path and replace the user overrides.
    pub fn reload(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        self.set_user_bindings(Self::load_from_file(&path));
    }

    /// The ordered set of definitions.
    #[must_use]
    pub fn definitions(&self) -> &[KeybindingDefinition] {
        &self.definitions
    }

    /// Replace the user overrides and rebuild indices and conflicts.
    pub fn set_user_bindings(&mut self, user_bindings: BTreeMap<String, Vec<String>>) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    /// The current user overrides.
    #[must_use]
    pub fn user_bindings(&self) -> &BTreeMap<String, Vec<String>> {
        &self.user_bindings
    }

    /// Resolved keys for one id, or an empty slice if the id is unknown.
    #[must_use]
    pub fn get_keys(&self, id: &str) -> &[String] {
        self.keys_by_id.get(id).map_or(&[], Vec::as_slice)
    }

    /// Conflicts detected across user bindings.
    #[must_use]
    pub fn get_conflicts(&self) -> &[KeybindingConflict] {
        &self.conflicts
    }

    /// Whether `key` matches any resolved key for `id`.
    #[must_use]
    pub fn matches(&self, key: &KeyEvent, id: &str) -> bool {
        let event_id = key_event_id(key);
        self.matches_key_id(&event_id, id)
    }

    /// Whether a precomputed canonical key id matches `id`.
    #[must_use]
    pub fn matches_key_id(&self, event_id: &str, id: &str) -> bool {
        if event_id.is_empty() {
            return false;
        }
        self.get_keys(id)
            .iter()
            .any(|key| normalize_key_id(key) == event_id)
    }

    /// Every id with its resolved keys.
    #[must_use]
    #[cfg(test)]
    pub fn get_resolved_bindings(&self) -> BTreeMap<String, Vec<String>> {
        self.definitions
            .iter()
            .map(|definition| {
                (
                    definition.id.clone(),
                    self.get_keys(&definition.id).to_vec(),
                )
            })
            .collect()
    }

    /// Whether `id` is part of the registry.
    #[must_use]
    pub fn has_definition(&self, id: &str) -> bool {
        self.definitions
            .iter()
            .any(|definition| definition.id == id)
    }

    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        let mut claims: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (id, keys) in &self.user_bindings {
            if !self.has_definition(id) {
                continue;
            }
            for key in normalize_keys(keys) {
                claims.entry(key).or_default().insert(id.clone());
            }
        }
        for (key, ids) in &claims {
            if ids.len() > 1 {
                self.conflicts.push(KeybindingConflict {
                    key: key.clone(),
                    keybindings: ids.iter().cloned().collect(),
                });
            }
        }

        for definition in &self.definitions {
            let keys = match self.user_bindings.get(&definition.id) {
                Some(user_keys) => normalize_keys(user_keys),
                None => normalize_keys(&definition.default_keys),
            };
            self.keys_by_id.insert(definition.id.clone(), keys);
        }
    }

    fn load_from_file(path: &Path) -> BTreeMap<String, Vec<String>> {
        let Some(raw) = load_raw_config(path) else {
            return BTreeMap::new();
        };
        let (migrated, _) = migrate_keybindings_config(&raw);
        to_keybindings_config(&migrated)
    }
}

fn normalize_keys(keys: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for key in keys {
        let normalized = normalize_key_id(key);
        if !normalized.is_empty() && seen.insert(normalized.clone()) {
            result.push(normalized);
        }
    }
    result
}

/// Legacy flat keybinding names mapped to their namespaced ids.
pub const KEYBINDING_NAME_MIGRATIONS: &[(&str, &str)] = &[
    ("cursorUp", "tui.editor.cursorUp"),
    ("cursorDown", "tui.editor.cursorDown"),
    ("cursorLeft", "tui.editor.cursorLeft"),
    ("cursorRight", "tui.editor.cursorRight"),
    ("cursorWordLeft", "tui.editor.cursorWordLeft"),
    ("cursorWordRight", "tui.editor.cursorWordRight"),
    ("cursorLineStart", "tui.editor.cursorLineStart"),
    ("cursorLineEnd", "tui.editor.cursorLineEnd"),
    ("jumpForward", "tui.editor.jumpForward"),
    ("jumpBackward", "tui.editor.jumpBackward"),
    ("pageUp", "tui.editor.pageUp"),
    ("pageDown", "tui.editor.pageDown"),
    ("deleteCharBackward", "tui.editor.deleteCharBackward"),
    ("deleteCharForward", "tui.editor.deleteCharForward"),
    ("deleteWordBackward", "tui.editor.deleteWordBackward"),
    ("deleteWordForward", "tui.editor.deleteWordForward"),
    ("deleteToLineStart", "tui.editor.deleteToLineStart"),
    ("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
    ("yank", "tui.editor.yank"),
    ("yankPop", "tui.editor.yankPop"),
    ("undo", "tui.editor.undo"),
    ("newLine", "tui.input.newLine"),
    ("submit", "tui.input.submit"),
    ("tab", "tui.input.tab"),
    ("copy", "tui.input.copy"),
    ("selectUp", "tui.select.up"),
    ("selectDown", "tui.select.down"),
    ("selectPageUp", "tui.select.pageUp"),
    ("selectPageDown", "tui.select.pageDown"),
    ("selectConfirm", "tui.select.confirm"),
    ("selectCancel", "tui.select.cancel"),
    ("interrupt", "app.interrupt"),
    ("clear", "app.clear"),
    ("exit", "app.exit"),
    ("suspend", "app.suspend"),
    ("cycleThinkingLevel", "app.thinking.cycle"),
    ("cycleModelForward", "app.model.cycleForward"),
    ("cycleModelBackward", "app.model.cycleBackward"),
    ("selectModel", "app.model.select"),
    ("expandTools", "app.tools.expand"),
    ("toggleThinking", "app.thinking.toggle"),
    ("toggleSessionNamedFilter", "app.session.toggleNamedFilter"),
    ("externalEditor", "app.editor.external"),
    ("followUp", "app.message.followUp"),
    ("dequeue", "app.message.dequeue"),
    ("pasteImage", "app.clipboard.pasteImage"),
    ("newSession", "app.session.new"),
    ("fork", "app.session.fork"),
    ("resume", "app.session.resume"),
    ("toggleSessionPath", "app.session.togglePath"),
    ("toggleSessionSort", "app.session.toggleSort"),
    ("renameSession", "app.session.rename"),
    ("deleteSession", "app.session.delete"),
    ("deleteSessionNoninvasive", "app.session.deleteNoninvasive"),
];

/// Read a keybindings JSON object from disk, ignoring a missing or malformed
/// file exactly like upstream.
#[must_use]
pub fn load_raw_config(path: &Path) -> Option<serde_json::Map<String, Value>> {
    // Resolve the trusted user directory, but never follow a linked final file.
    let path = path.parent()?.canonicalize().ok()?.join(path.file_name()?);
    let raw = String::from_utf8(
        octet_agent::secure_fs::read_regular_file_bounded(&path, 256 * 1024).ok()?,
    )
    .ok()?;
    let stripped = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    match serde_json::from_str::<Value>(stripped) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// Rewrite legacy flat names to namespaced ids.
///
/// Returns the migrated object and whether anything changed. When both a
/// legacy name and its namespaced target are present, the namespaced value
/// wins and the legacy entry is dropped (matching upstream).
#[must_use]
pub fn migrate_keybindings_config(
    raw: &serde_json::Map<String, Value>,
) -> (serde_json::Map<String, Value>, bool) {
    let mut migrated = false;
    let mut config = serde_json::Map::new();
    for (key, value) in raw {
        let next_key = KEYBINDING_NAME_MIGRATIONS
            .iter()
            .find(|(legacy, _)| legacy == key)
            .map_or(key.as_str(), |(_, namespaced)| *namespaced);
        if next_key != key {
            migrated = true;
            if raw.contains_key(next_key) {
                continue;
            }
        }
        config.insert(next_key.to_owned(), value.clone());
    }
    (order_keybindings_config(&config), migrated)
}

fn order_keybindings_config(
    config: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut ordered = serde_json::Map::new();
    for (id, _, _) in BASE_DEFINITIONS {
        if let Some(value) = config.get(*id) {
            ordered.insert((*id).to_owned(), value.clone());
        }
    }
    let mut extras: Vec<&String> = config
        .keys()
        .filter(|key| !ordered.contains_key(key.as_str()))
        .collect();
    extras.sort();
    for key in extras {
        ordered.insert(key.clone(), config[key].clone());
    }
    ordered
}

/// Filter a raw config object down to string and string-array values.
#[must_use]
pub fn to_keybindings_config(
    raw: &serde_json::Map<String, Value>,
) -> BTreeMap<String, Vec<String>> {
    let mut config = BTreeMap::new();
    for (key, value) in raw {
        let keys = match value {
            Value::String(single) => Some(vec![single.clone()]),
            Value::Array(entries) => entries
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<&str>>>()
                .map(|entries| entries.into_iter().map(str::to_owned).collect()),
            _ => None,
        };
        if let Some(keys) = keys {
            config.insert(key.clone(), keys);
        }
    }
    config
}

// The ordered registry: every `tui.*` default followed by the additive `app.*`
// overlay. Platform overrides are applied by `default_definitions`.
#[rustfmt::skip]
const BASE_DEFINITIONS: &[(&str, &[&str], &str)] = &[
    ("tui.editor.cursorUp", &["up"], "Move cursor up"),
    ("tui.editor.cursorDown", &["down"], "Move cursor down"),
    ("tui.editor.historyPrevious", &[], "Select previous prompt history entry"),
    ("tui.editor.historyNext", &[], "Select next prompt history entry"),
    ("tui.editor.cursorLeft", &["left", "ctrl+b"], "Move cursor left"),
    ("tui.editor.cursorRight", &["right", "ctrl+f"], "Move cursor right"),
    ("tui.editor.cursorWordLeft", &["alt+left", "ctrl+left", "alt+b"], "Move cursor word left"),
    ("tui.editor.cursorWordRight", &["alt+right", "ctrl+right", "alt+f"], "Move cursor word right"),
    ("tui.editor.cursorLineStart", &["home", "ctrl+home", "ctrl+a"], "Move to line start"),
    ("tui.editor.cursorLineEnd", &["end", "ctrl+end", "ctrl+e"], "Move to line end"),
    ("tui.editor.jumpForward", &["ctrl+]"], "Jump forward to character"),
    ("tui.editor.jumpBackward", &["ctrl+alt+]"], "Jump backward to character"),
    ("tui.editor.pageUp", &["pageup", "ctrl+pageup"], "Page up"),
    ("tui.editor.pageDown", &["pagedown", "ctrl+pagedown"], "Page down"),
    ("tui.editor.deleteCharBackward", &["backspace"], "Delete character backward"),
    ("tui.editor.deleteCharForward", &["delete", "ctrl+d"], "Delete character forward"),
    ("tui.editor.deleteWordBackward", &["ctrl+w", "alt+backspace"], "Delete word backward"),
    ("tui.editor.deleteWordForward", &["alt+d", "alt+delete"], "Delete word forward"),
    ("tui.editor.deleteToLineStart", &["ctrl+u"], "Delete to line start"),
    ("tui.editor.deleteToLineEnd", &["ctrl+k"], "Delete to line end"),
    ("tui.editor.yank", &["ctrl+y"], "Yank"),
    ("tui.editor.yankPop", &["alt+y"], "Yank pop"),
    ("tui.editor.undo", &["ctrl+-"], "Undo"),
    ("tui.editor.redo", &["ctrl+shift+-"], "Redo"),
    ("tui.input.newLine", &["shift+enter", "ctrl+j"], "Insert newline"),
    ("tui.input.submit", &["enter"], "Submit input"),
    ("tui.input.tab", &["tab"], "Tab / autocomplete"),
    ("tui.input.copy", &["ctrl+c"], "Copy selection"),
    ("tui.select.up", &["up"], "Move selection up"),
    ("tui.select.down", &["down"], "Move selection down"),
    ("tui.select.pageUp", &["pageup"], "Selection page up"),
    ("tui.select.pageDown", &["pagedown"], "Selection page down"),
    ("tui.select.confirm", &["enter"], "Confirm selection"),
    ("tui.select.cancel", &["escape", "ctrl+c"], "Cancel selection"),
    ("tui.altScreen.pageUp", &["pageup"], "Scroll viewport up one page"),
    ("tui.altScreen.pageDown", &["pagedown"], "Scroll viewport down one page"),
    ("tui.altScreen.halfPageUp", &[], "Scroll viewport up half a page"),
    ("tui.altScreen.halfPageDown", &[], "Scroll viewport down half a page"),
    ("tui.altScreen.lineUp", &[], "Scroll viewport up one line"),
    ("tui.altScreen.lineDown", &[], "Scroll viewport down one line"),
    ("tui.altScreen.previousPrompt", &["ctrl+shift+up", "ctrl+up"], "Jump to previous semantic prompt"),
    ("tui.altScreen.nextPrompt", &["ctrl+shift+down", "ctrl+down"], "Jump to next semantic prompt"),
    ("tui.altScreen.search", &["ctrl+shift+f"], "Search the primary scroll view"),
    ("tui.altScreen.toggleScrollbar", &["ctrl+shift+b"], "Cycle transcript scrollbar hidden/auto/always"),
    ("tui.altScreen.searchNext", &["enter", "ctrl+g"], "Select the next search match"),
    ("tui.altScreen.searchPrevious", &["shift+enter", "ctrl+shift+g"], "Select the previous search match"),
    ("tui.altScreen.searchClose", &["escape"], "Close transcript search"),
    ("tui.altScreen.top", &["home"], "Scroll viewport to top"),
    ("tui.altScreen.bottom", &["end"], "Scroll viewport to bottom"),
    ("app.interrupt", &["escape"], "Cancel or abort"),
    ("app.clear", &["ctrl+c"], "Clear editor"),
    ("app.exit", &["ctrl+d"], "Exit when editor is empty"),
    ("app.suspend", &["ctrl+z"], "Suspend to background"),
    ("app.thinking.cycle", &["shift+tab"], "Cycle thinking level"),
    ("app.thinking.save", &["ctrl+s"], "Save thinking level"),
    ("app.model.cycleForward", &["ctrl+p"], "Cycle to next model"),
    ("app.model.cycleBackward", &["shift+ctrl+p"], "Cycle to previous model"),
    ("app.model.select", &["ctrl+l"], "Open model selector"),
    ("app.tools.expand", &["ctrl+o"], "Toggle tool output"),
    ("app.thinking.toggle", &["ctrl+t"], "Toggle thinking blocks"),
    ("app.session.toggleNamedFilter", &["ctrl+n"], "Toggle named session filter"),
    ("app.editor.external", &["ctrl+g"], "Open external editor"),
    ("app.message.copy", &["ctrl+x"], "Copy message to clipboard"),
    ("app.message.followUp", &["alt+enter"], "Queue follow-up message"),
    ("app.message.dequeue", &["alt+up"], "Restore queued messages"),
    ("app.clipboard.pasteImage", &["ctrl+v"], "Paste image from clipboard (text fallback)"),
    ("app.session.new", &[], "Start a new session"),
    ("app.session.fork", &[], "Fork current session"),
    ("app.session.resume", &[], "Resume a session"),
    ("app.session.togglePath", &["ctrl+p"], "Toggle session path display"),
    ("app.session.toggleSort", &["ctrl+s"], "Toggle session sort mode"),
    ("app.session.search", &["ctrl+f"], "Search session transcripts for the query"),
    ("app.session.rename", &["ctrl+r"], "Rename session"),
    ("app.session.delete", &["ctrl+d"], "Delete session"),
    ("app.session.deleteNoninvasive", &["ctrl+backspace"], "Delete session when query is empty"),
    ("app.models.save", &["ctrl+s"], "Save model selection"),
    ("app.models.enableAll", &["ctrl+a"], "Enable all models"),
    ("app.models.clearAll", &["ctrl+x"], "Clear all models"),
    ("app.models.toggleProvider", &["ctrl+p"], "Toggle all models for provider"),
    ("app.models.reorderUp", &["alt+up"], "Move model up in order"),
    ("app.models.reorderDown", &["alt+down"], "Move model down in order"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn user(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(id, keys)| ((*id).to_owned(), strings(keys)))
            .collect()
    }

    #[test]
    fn plus_backtab_and_oversized_files_are_handled_at_the_boundary() {
        assert_eq!(normalize_key_id("shift+ctrl++"), "ctrl+shift++");
        assert_eq!(
            key_event_id(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
            "shift+tab"
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keybindings.json");
        std::fs::write(&path, " ".repeat(256 * 1024 + 1)).unwrap();
        assert!(load_raw_config(&path).is_none());
        assert!(load_raw_config(directory.path()).is_none());
    }

    #[test]
    fn defaults_are_namespaced_and_ordered() {
        let manager = KeybindingsManager::with_platform("macos", false, BTreeMap::new());
        assert!(manager.has_definition("tui.editor.undo"));
        assert!(manager.has_definition("app.message.followUp"));
        assert_eq!(manager.get_keys("tui.editor.undo"), ["ctrl+-"]);
        assert_eq!(
            manager.get_keys("tui.editor.deleteWordBackward"),
            ["ctrl+w", "alt+backspace"]
        );
        assert!(manager.get_keys("tui.editor.historyPrevious").is_empty());
    }

    #[test]
    fn platform_defaults_match_windows_and_wsl_behaviour() {
        let win = KeybindingsManager::with_platform("win32", false, BTreeMap::new());
        assert_eq!(win.get_keys("tui.editor.undo"), ["ctrl+z"]);
        assert!(win.get_keys("app.suspend").is_empty());
        assert_eq!(win.get_keys("tui.altScreen.search"), ["ctrl+f"]);
        assert_eq!(win.get_keys("app.message.followUp"), ["ctrl+q"]);

        let wsl = KeybindingsManager::with_platform("linux", true, BTreeMap::new());
        assert_eq!(wsl.get_keys("tui.editor.undo"), ["alt+z"]);
        assert_eq!(wsl.get_keys("app.clipboard.pasteImage"), ["alt+v"]);

        let plain = KeybindingsManager::with_platform("linux", false, BTreeMap::new());
        assert_eq!(plain.get_keys("tui.editor.undo"), ["ctrl+-"]);
        assert_eq!(
            plain.get_keys("tui.altScreen.previousPrompt"),
            ["ctrl+shift+up", "ctrl+up"]
        );

        assert!(use_windows_keybindings("win32", false));
        assert!(use_windows_keybindings("linux", true));
        assert!(!use_windows_keybindings("linux", false));
        assert!(!use_windows_keybindings("darwin", false));
    }

    #[test]
    fn user_overrides_replace_defaults_and_disable_on_empty_list() {
        let manager = KeybindingsManager::with_platform(
            "linux",
            false,
            user(&[
                ("tui.editor.undo", &["ctrl+z"]),
                ("app.exit", &[]),
                ("unknown.binding", &["ctrl+q"]),
            ]),
        );
        assert_eq!(manager.get_keys("tui.editor.undo"), ["ctrl+z"]);
        assert!(manager.get_keys("app.exit").is_empty());
        // Unknown ids are ignored entirely.
        assert!(!manager.has_definition("unknown.binding"));
        // Untouched ids keep their defaults.
        assert_eq!(manager.get_keys("tui.editor.yank"), ["ctrl+y"]);
    }

    #[test]
    fn conflicts_are_reported_for_keys_shared_between_bindings() {
        let manager = KeybindingsManager::with_platform(
            "linux",
            false,
            user(&[
                ("tui.editor.undo", &["ctrl+z"]),
                ("tui.editor.yank", &["ctrl+z"]),
                ("tui.editor.yankPop", &["alt+z"]),
            ]),
        );
        assert_eq!(
            manager.get_conflicts(),
            &[KeybindingConflict {
                key: "ctrl+z".to_owned(),
                keybindings: vec!["tui.editor.undo".to_owned(), "tui.editor.yank".to_owned()],
            }]
        );

        // A single claimant is not a conflict.
        let clean = KeybindingsManager::with_platform(
            "linux",
            false,
            user(&[("tui.editor.undo", &["ctrl+z"])]),
        );
        assert!(clean.get_conflicts().is_empty());
    }

    #[test]
    fn matches_normalizes_spelling_and_modifier_order() {
        let manager = KeybindingsManager::with_platform(
            "linux",
            false,
            user(&[("tui.altScreen.previousPrompt", &["shift+ctrl+up"])]),
        );
        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        assert!(manager.matches(&key, "tui.altScreen.previousPrompt"));
        assert!(!manager.matches(
            &KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL),
            "tui.altScreen.previousPrompt"
        ));
        // Esc and escape are the same key.
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            KeybindingsManager::with_platform("linux", false, BTreeMap::new())
                .matches(&escape, "app.interrupt")
        );
    }

    #[test]
    fn resolved_bindings_cover_every_definition() {
        let manager = KeybindingsManager::with_platform("linux", false, BTreeMap::new());
        let resolved = manager.get_resolved_bindings();
        assert_eq!(resolved.len(), manager.definitions().len());
        assert_eq!(
            resolved.get("tui.editor.undo").map(Vec::as_slice),
            Some(["ctrl+-".to_owned()].as_slice())
        );
    }

    #[test]
    fn legacy_flat_names_migrate_to_namespaced_ids() {
        let raw: serde_json::Map<String, Value> = serde_json::from_str(
            r#"{"undo":"ctrl+z","yank":"ctrl+y","app.interrupt":"ctrl+q","custom.key":"f1"}"#,
        )
        .unwrap();
        let (migrated, changed) = migrate_keybindings_config(&raw);
        assert!(changed);
        assert!(migrated.contains_key("tui.editor.undo"));
        assert!(migrated.contains_key("tui.editor.yank"));
        assert!(migrated.contains_key("app.interrupt"));
        assert!(migrated.contains_key("custom.key"));
        assert!(!migrated.contains_key("undo"));

        let config = to_keybindings_config(&migrated);
        assert_eq!(
            config.get("tui.editor.undo").map(Vec::as_slice),
            Some(["ctrl+z".to_owned()].as_slice())
        );
        assert_eq!(
            config.get("custom.key").map(Vec::as_slice),
            Some(["f1".to_owned()].as_slice())
        );
    }

    #[test]
    fn namespaced_value_wins_over_legacy_duplicate() {
        let raw: serde_json::Map<String, Value> =
            serde_json::from_str(r#"{"undo":"ctrl+z","tui.editor.undo":"ctrl+y"}"#).unwrap();
        let (migrated, changed) = migrate_keybindings_config(&raw);
        assert!(changed);
        let config = to_keybindings_config(&migrated);
        assert_eq!(
            config.get("tui.editor.undo").map(Vec::as_slice),
            Some(["ctrl+y".to_owned()].as_slice())
        );
    }

    #[test]
    fn load_from_file_reads_keybindings_json_and_ignores_non_objects() {
        let dir = std::env::temp_dir().join(format!("octet-kb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keybindings.json");
        std::fs::write(&path, "\u{feff}{\"undo\":\"ctrl+z\"}").unwrap();

        let loaded = KeybindingsManager::create(&dir, "linux", false);
        assert_eq!(
            loaded.get_keys("tui.editor.undo"),
            ["ctrl+z"],
            "BOM-prefixed legacy config should load and migrate"
        );

        std::fs::write(&path, "[1,2,3]").unwrap();
        let invalid = KeybindingsManager::create(&dir, "linux", false);
        assert_eq!(invalid.get_keys("tui.editor.undo"), ["ctrl+-"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reload_replaces_user_overrides() {
        let dir = std::env::temp_dir().join(format!("octet-kb-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keybindings.json");
        std::fs::write(&path, r#"{"undo":"ctrl+z"}"#).unwrap();
        let mut manager = KeybindingsManager::create(&dir, "linux", false);
        assert_eq!(manager.get_keys("tui.editor.undo"), ["ctrl+z"]);
        std::fs::write(&path, r#"{"undo":"ctrl+y"}"#).unwrap();
        manager.reload();
        assert_eq!(manager.get_keys("tui.editor.undo"), ["ctrl+y"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}

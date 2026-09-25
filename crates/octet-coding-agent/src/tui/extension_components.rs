//! Host side of the API `0.4` component line-region protocol.
//!
//! Pi is the spec (`packages/tui/src/tui.ts` `Component` interface ~line 111):
//! a component owns `render(width) -> string[]`, optional `handleInput(data)`
//! and `handleMouse(event)`, and `invalidate()`. Octet's out-of-process bridge
//! (W4) instantiates the component factories; this module reserves one screen
//! region per live component id, tracks invalidated ids with a per-frame
//! coalescer, and folds rendered lines into the shell projection at the
//! pi placements (`packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! `extensionWidgetsAbove`/`extensionWidgetsBelow`/header/footer/editor).
//!
//! The wire operations are owned by the octet-agent protocol layer (W1:
//! `component/invalidate`, `ui/widget_set`, `ui/header_set`, `ui/footer_set`,
//! `ui/editor_component_set/get`, `ui/working_*`, `ui/hidden_thinking_label_set`,
//! `ui/tools_expanded_get/set`, `terminal/title_set` and the host→bridge
//! `component/render|input|mouse|dispose` requests). This module deliberately
//! mirrors the contract's exact field names so the W1 drain is a mechanical
//! adapter, and keeps every bound from the wire contract §3 so bridge payloads
//! can be validated before they reserve screen state.

use std::collections::BTreeMap;

/// Wire bound: component id bytes.
pub const MAX_COMPONENT_ID_BYTES: usize = 64;
/// Wire bound: live components per extension generation.
pub const MAX_LIVE_COMPONENTS: usize = 16;
/// Wire bound: `component/render` response lines.
pub const MAX_RENDER_LINES: usize = 256;
/// Wire bound: one `component/render` response line bytes.
pub const MAX_RENDER_LINE_BYTES: usize = 16 * 1024;
/// Wire bound: `ui/widget_set` lines-form line count.
pub const MAX_WIDGET_LINES: usize = 64;
/// Wire bound: one `ui/widget_set` lines-form line bytes.
pub const MAX_WIDGET_LINE_BYTES: usize = 8 * 1024;
/// Wire bound: `ui/widget_set` key bytes.
pub const MAX_WIDGET_KEY_BYTES: usize = 64;
/// Wire bound: terminal title bytes.
pub const MAX_TERMINAL_TITLE_BYTES: usize = 8 * 1024;

/// Pi widget placement (`ExtensionWidgetOptions.placement`,
/// `core/extensions/types.ts:105`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WidgetPlacement {
    /// Default: rendered above the editor.
    #[default]
    AboveEditor,
    /// Rendered below the editor.
    BelowEditor,
}

impl WidgetPlacement {
    /// Parse the wire spelling (`"aboveEditor" | "belowEditor"`).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "aboveEditor" => Some(Self::AboveEditor),
            "belowEditor" => Some(Self::BelowEditor),
            _ => None,
        }
    }
}

/// One widget slot: either verbatim lines (pi's `string[]` overload) or a live
/// component region (pi's factory overload).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WidgetContent {
    /// Complete plain-text lines for the key.
    Lines(Vec<String>),
    /// Component factory handle; painted from the reserved region.
    Component(String),
}

/// A typed, bounded component-surface error mirroring the protocol layer's
/// typed `bounds_exceeded` refusals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComponentSurfaceError {
    /// Identifier or payload exceeded its wire bound.
    BoundsExceeded(String),
    /// More live components than the wire bound allows.
    TooManyLiveComponents,
}

impl std::fmt::Display for ComponentSurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BoundsExceeded(detail) => {
                write!(f, "component surface bounds exceeded: {detail}")
            }
            Self::TooManyLiveComponents => {
                write!(f, "more than {MAX_LIVE_COMPONENTS} live components")
            }
        }
    }
}

impl std::error::Error for ComponentSurfaceError {}

fn bounded_id(id: &str) -> Result<(), ComponentSurfaceError> {
    if id.is_empty() || id.len() > MAX_COMPONENT_ID_BYTES {
        return Err(ComponentSurfaceError::BoundsExceeded(format!(
            "component id must be 1..={MAX_COMPONENT_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn bounded_lines(
    lines: &[String],
    max_lines: usize,
    max_line_bytes: usize,
    label: &str,
) -> Result<(), ComponentSurfaceError> {
    if lines.len() > max_lines {
        return Err(ComponentSurfaceError::BoundsExceeded(format!(
            "{label} has {} lines; limit is {max_lines}",
            lines.len()
        )));
    }
    for line in lines {
        if line.len() > max_line_bytes {
            return Err(ComponentSurfaceError::BoundsExceeded(format!(
                "{label} line is {} bytes; limit is {max_line_bytes}",
                line.len()
            )));
        }
    }
    Ok(())
}

/// One reserved screen region for a live component.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComponentRegion {
    /// Region width of the last accepted render, if any.
    pub width: Option<u16>,
    /// Last rendered lines (may be stale until the render round-trip lands).
    pub lines: Vec<String>,
}

/// The per-extension component surface. One instance per extension process
/// generation; stale generations are dropped wholesale by the drain.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtensionComponentSurface {
    /// Live component ids and their reserved regions.
    components: BTreeMap<String, ComponentRegion>,
    /// Component ids invalidated since the last frame drain. Debounced per
    /// the wire contract: repeated `component/invalidate` for one id collapses
    /// to a single `component/render` request.
    invalidated: Vec<String>,
    /// Keyed widget slots in wire-arrival order.
    widgets: BTreeMap<String, (WidgetContent, WidgetPlacement)>,
    /// Header component id (`ui/header_set`; absent = built-in header).
    header: Option<String>,
    /// Footer component id (`ui/footer_set`; absent = built-in footer).
    footer: Option<String>,
    /// Editor component id (`ui/editor_component_set`; absent = default editor).
    editor_component: Option<String>,
    /// `ui/tools_expanded_get/set` state.
    tools_expanded: bool,
    /// `terminal/title_set` payload; the shell writes the OSC 2 sequence.
    terminal_title: Option<String>,
    /// `component/render` requests queued by the repaint loop, newest last.
    /// The drain forwards each as the host→bridge `component/render` request
    /// and stores the response with [`Self::store_render`].
    pending_renders: Vec<(String, u16)>,
}

impl ExtensionComponentSurface {
    /// Mark one component region dirty. Coalesced: an id already awaiting a
    /// render is not queued twice.
    pub fn invalidate(&mut self, id: &str) -> Result<(), ComponentSurfaceError> {
        bounded_id(id)?;
        if !self.components.contains_key(id) {
            // A live component bound arrives with its first region claim; the
            // wire `component/invalidate` for an unknown id still reserves the
            // region so the first render request can flow.
            if self.components.len() >= MAX_LIVE_COMPONENTS {
                return Err(ComponentSurfaceError::TooManyLiveComponents);
            }
            self.components
                .insert(id.to_owned(), ComponentRegion::default());
        }
        if !self.invalidated.iter().any(|pending| pending == id) {
            self.invalidated.push(id.to_owned());
        }
        Ok(())
    }

    /// Claim a live component region explicitly (component-form widget,
    /// header/footer/editor factories). Idempotent per generation.
    pub fn register_component(&mut self, id: &str) -> Result<(), ComponentSurfaceError> {
        bounded_id(id)?;
        if self.components.contains_key(id) {
            return Ok(());
        }
        if self.components.len() >= MAX_LIVE_COMPONENTS {
            return Err(ComponentSurfaceError::TooManyLiveComponents);
        }
        self.components
            .insert(id.to_owned(), ComponentRegion::default());
        Ok(())
    }

    /// Take the ids invalidated since the last frame and queue one
    /// `component/render {id, width}` request per id. Called from the shell
    /// repaint loop at frame granularity, exactly once per frame.
    pub fn queue_renders_for_frame(&mut self, width: u16) {
        for id in std::mem::take(&mut self.invalidated) {
            self.pending_renders.push((id, width));
        }
    }

    /// Drain the queued render requests. Each entry is one host→bridge
    /// `component/render` request; the response is stored with
    /// [`Self::store_render`].
    pub fn take_pending_renders(&mut self) -> Vec<(String, u16)> {
        std::mem::take(&mut self.pending_renders)
    }

    /// Store one `component/render` response. Bounded per the wire contract
    /// (§4: ≤256 lines × 16 KiB). Returns whether the surface changed.
    pub fn store_render(
        &mut self,
        id: &str,
        width: u16,
        lines: Vec<String>,
    ) -> Result<bool, ComponentSurfaceError> {
        bounded_id(id)?;
        bounded_lines(
            &lines,
            MAX_RENDER_LINES,
            MAX_RENDER_LINE_BYTES,
            "component render",
        )?;
        match self.components.get_mut(id) {
            Some(region) => {
                let changed = region.width != Some(width) || region.lines != lines;
                region.width = Some(width);
                region.lines = lines;
                Ok(changed)
            }
            None => Err(ComponentSurfaceError::BoundsExceeded(format!(
                "component {id:?} has no reserved region"
            ))),
        }
    }

    /// `ui/widget_set`: exactly one of `component_id`/`lines`, neither clears
    /// the key. Key ≤64B; lines ≤64 × 8KiB; placement defaults to aboveEditor.
    pub fn widget_set(
        &mut self,
        key: &str,
        content: Option<WidgetContent>,
        placement: Option<WidgetPlacement>,
    ) -> Result<(), ComponentSurfaceError> {
        if key.is_empty() || key.len() > MAX_WIDGET_KEY_BYTES {
            return Err(ComponentSurfaceError::BoundsExceeded(format!(
                "widget key must be 1..={MAX_WIDGET_KEY_BYTES} bytes"
            )));
        }
        match content {
            None => {
                self.widgets.remove(key);
                Ok(())
            }
            Some(WidgetContent::Lines(lines)) => {
                bounded_lines(&lines, MAX_WIDGET_LINES, MAX_WIDGET_LINE_BYTES, "widget")?;
                self.widgets.insert(
                    key.to_owned(),
                    (WidgetContent::Lines(lines), placement.unwrap_or_default()),
                );
                Ok(())
            }
            Some(WidgetContent::Component(id)) => {
                self.register_component(&id)?;
                self.widgets.insert(
                    key.to_owned(),
                    (WidgetContent::Component(id), placement.unwrap_or_default()),
                );
                Ok(())
            }
        }
    }

    /// `ui/header_set` / `ui/footer_set`. Absent id restores the built-in
    /// surface.
    pub fn slot_set(
        &mut self,
        slot: ComponentSlot,
        id: Option<&str>,
    ) -> Result<(), ComponentSurfaceError> {
        if let Some(id) = id {
            bounded_id(id)?;
            self.register_component(id)?;
        }
        let target = match slot {
            ComponentSlot::Header => &mut self.header,
            ComponentSlot::Footer => &mut self.footer,
            ComponentSlot::Editor => &mut self.editor_component,
        };
        *target = id.map(str::to_owned);
        Ok(())
    }

    /// `ui/editor_component_get`.
    pub fn editor_component_id(&self) -> Option<&str> {
        self.editor_component.as_deref()
    }

    /// `ui/header_component_id` (host-local mirror of `ui/header_set`).
    pub fn header_component_id(&self) -> Option<&str> {
        self.header.as_deref()
    }

    /// `ui/footer_component_id` (host-local mirror of `ui/footer_set`).
    pub fn footer_component_id(&self) -> Option<&str> {
        self.footer.as_deref()
    }

    /// `ui/tools_expanded_get`.
    pub fn tools_expanded(&self) -> bool {
        self.tools_expanded
    }

    /// `ui/tools_expanded_set`.
    pub fn set_tools_expanded(&mut self, expanded: bool) {
        self.tools_expanded = expanded;
    }

    /// `terminal/title_set` (OSC 2). Absent title restores the host default.
    pub fn set_terminal_title(&mut self, title: Option<&str>) -> Result<(), ComponentSurfaceError> {
        if let Some(title) = title {
            if title.len() > MAX_TERMINAL_TITLE_BYTES {
                return Err(ComponentSurfaceError::BoundsExceeded(format!(
                    "terminal title is {} bytes; limit is {MAX_TERMINAL_TITLE_BYTES}",
                    title.len()
                )));
            }
        }
        self.terminal_title = title.map(str::to_owned);
        Ok(())
    }

    /// Current terminal title, for the OSC 2 writer in the shell chrome.
    pub fn terminal_title(&self) -> Option<&str> {
        self.terminal_title.as_deref()
    }

    /// `component/dispose` on clear/shutdown: drop every region and slot.
    pub fn clear(&mut self) {
        self.components.clear();
        self.invalidated.clear();
        self.widgets.clear();
        self.header = None;
        self.footer = None;
        self.editor_component = None;
        self.tools_expanded = false;
        self.terminal_title = None;
        self.pending_renders.clear();
    }

    /// The component receiving keyboard focus: the editor component when one
    /// is installed, else none (the default composer keeps focus). Mirrors
    /// pi's focus rule where the editor component replaces the composer.
    pub fn keyboard_focus_id(&self) -> Option<&str> {
        self.editor_component.as_deref()
    }

    /// One widget's region lines for projection, when the widget is backed by
    /// a component with a completed render.
    pub fn widget_component_lines(&self, key: &str) -> Option<&[String]> {
        let (content, _) = self.widgets.get(key)?;
        match content {
            WidgetContent::Component(id) => self.components.get(id).map(|region| &region.lines[..]),
            WidgetContent::Lines(_) => None,
        }
    }

    /// All widget slots with their placement, in key order, for the layout
    /// pass that reserves regions above/below the editor.
    pub fn widgets(&self) -> impl Iterator<Item = (&String, &WidgetContent, WidgetPlacement)> + '_ {
        self.widgets
            .iter()
            .map(|(key, (content, placement))| (key, content, *placement))
    }

    /// Project one component's rendered lines for a slot surface
    /// (header/footer/editor), if the region has painted at least once.
    pub fn slot_lines(&self, id: Option<&str>) -> Option<&[String]> {
        let id = id?;
        self.components.get(id).map(|region| &region.lines[..])
    }

    /// Lines-form widgets above the editor, in stable key order.
    pub fn widget_lines_above_editor(&self) -> Vec<(String, Vec<String>)> {
        self.widget_lines_for_placement(WidgetPlacement::AboveEditor)
    }

    /// Lines-form widgets below the editor, in stable key order.
    pub fn widget_lines_below_editor(&self) -> Vec<(String, Vec<String>)> {
        self.widget_lines_for_placement(WidgetPlacement::BelowEditor)
    }

    fn widget_lines_for_placement(&self, placement: WidgetPlacement) -> Vec<(String, Vec<String>)> {
        self.widgets
            .iter()
            .filter(|(_, (_, widget_placement))| *widget_placement == placement)
            .filter_map(|(key, (content, _))| match content {
                WidgetContent::Lines(lines) => Some((key.clone(), lines.clone())),
                WidgetContent::Component(id) => self
                    .components
                    .get(id)
                    .map(|region| (key.clone(), region.lines.clone())),
            })
            .collect()
    }

    /// Component ids whose regions should be dropped for a generation reset.
    pub fn component_ids(&self) -> impl Iterator<Item = &String> + '_ {
        self.components.keys()
    }
}

/// Addressable component slots (`ui/header_set`, `ui/footer_set`,
/// `ui/editor_component_set`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComponentSlot {
    /// Header region above the transcript.
    Header,
    /// Footer/status region below the editor.
    Footer,
    /// The composer editor region itself.
    Editor,
}

/// Encode one crossterm key event as the raw terminal sequence pi-tui
/// components expect from `handleInput(data)`. Pi components receive the raw
/// bytes the terminal produced; this restores the standard ANSI/Kitty-less
/// spellings for the keys octet decodes with crossterm.
pub fn raw_key_data(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> String {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    match code {
        KeyCode::Enter => "\r".to_owned(),
        KeyCode::Tab => "\t".to_owned(),
        KeyCode::BackTab => "\x1b[Z".to_owned(),
        KeyCode::Esc => "\x1b".to_owned(),
        KeyCode::Backspace => "\x7f".to_owned(),
        KeyCode::Left => "\x1b[D".to_owned(),
        KeyCode::Right => "\x1b[C".to_owned(),
        KeyCode::Up => "\x1b[A".to_owned(),
        KeyCode::Down => "\x1b[B".to_owned(),
        KeyCode::Home => "\x1b[H".to_owned(),
        KeyCode::End => "\x1b[F".to_owned(),
        KeyCode::PageUp => "\x1b[5~".to_owned(),
        KeyCode::PageDown => "\x1b[6~".to_owned(),
        KeyCode::Delete => "\x1b[3~".to_owned(),
        KeyCode::Insert => "\x1b[2~".to_owned(),
        KeyCode::F(number) => match number {
            1 => "\x1bOP".to_owned(),
            2 => "\x1bOQ".to_owned(),
            3 => "\x1bOR".to_owned(),
            4 => "\x1bOS".to_owned(),
            number @ 5..=12 => format!("\x1b[{number}~"),
            _ => String::new(),
        },
        KeyCode::Char(' ') if ctrl => "\x00".to_owned(),
        KeyCode::Char(character) => {
            let mut encoded = character.to_string();
            if ctrl {
                // Control characters are the lowercase letter's byte with the
                // high bits cleared, per the historic terminal convention.
                if let Some(lower) = character.to_ascii_lowercase().to_string().pop() {
                    encoded = (((lower as u8) & 0x1f) as char).to_string();
                }
            }
            if alt {
                encoded = format!("\x1b{encoded}");
            }
            if shift && character.is_lowercase() {
                encoded = encoded.to_uppercase();
            }
            encoded
        }
        KeyCode::Null => String::new(),
        _ => String::new(),
    }
}

/// The pi `TuiMouseEvent` wire shape (`packages/tui/src/tui.ts:25-44`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TuiMouseWireEvent {
    /// `"press" | "release" | "move" | "drag" | "click" | "wheel"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `"left" | "middle" | "right" | "none"`.
    pub button: String,
    /// Coordinates local to the receiving component.
    pub x: u16,
    pub y: u16,
    /// Absolute terminal coordinates.
    #[serde(rename = "screenX")]
    pub screen_x: u16,
    #[serde(rename = "screenY")]
    pub screen_y: u16,
    /// Current component bounds.
    pub width: u16,
    pub height: u16,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    /// Logical lines; negative values scroll up (wheel events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "wheelDelta")]
    pub wheel_delta: Option<i32>,
    /// Consecutive click count when type is click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "clickCount")]
    pub click_count: Option<u32>,
}

/// Map one crossterm mouse event into pi's normalized shape with local
/// coordinates for the region at `(origin_x, origin_y)` sized
/// `(width, height)`. crossterm coordinates are 1-based cell coordinates;
/// pi's are zero-based (`tui.ts:24`).
pub fn mouse_wire_event(
    event: &crossterm::event::MouseEvent,
    origin: (u16, u16),
    size: (u16, u16),
) -> TuiMouseWireEvent {
    use crossterm::event::MouseEventKind as Kind;
    let (origin_x, origin_y) = origin;
    let (width, height) = size;
    let (kind, button_name, wheel_delta) = match event.kind {
        Kind::Down(button) => ("press", button_name(button), None),
        Kind::Up(button) => ("release", button_name(button), None),
        Kind::Drag(button) => ("drag", button_name(button), None),
        Kind::Moved => ("move", "none", None),
        Kind::ScrollDown => ("wheel", "none", Some(-1)),
        Kind::ScrollUp => ("wheel", "none", Some(1)),
        Kind::ScrollLeft => ("wheel", "none", Some(0)),
        Kind::ScrollRight => ("wheel", "none", Some(0)),
    };
    TuiMouseWireEvent {
        kind: kind.to_owned(),
        button: button_name.to_owned(),
        x: event.column.saturating_sub(origin_x).saturating_sub(1),
        y: event.row.saturating_sub(origin_y).saturating_sub(1),
        screen_x: event.column.saturating_sub(1),
        screen_y: event.row.saturating_sub(1),
        width,
        height,
        shift: event
            .modifiers
            .contains(crossterm::event::KeyModifiers::SHIFT),
        alt: event
            .modifiers
            .contains(crossterm::event::KeyModifiers::ALT),
        ctrl: event
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL),
        wheel_delta,
        click_count: None,
    }
}

fn button_name(button: crossterm::event::MouseButton) -> &'static str {
    use crossterm::event::MouseButton as Button;
    match button {
        Button::Left => "left",
        Button::Middle => "middle",
        Button::Right => "right",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface() -> ExtensionComponentSurface {
        ExtensionComponentSurface::default()
    }

    #[test]
    fn invalidate_coalesces_repeated_ids_into_one_render_request() {
        let mut surface = surface();
        surface.invalidate("editor.1").unwrap();
        surface.invalidate("editor.1").unwrap();
        surface.invalidate("editor.1").unwrap();
        surface.queue_renders_for_frame(80);
        let pending = surface.take_pending_renders();
        assert_eq!(pending, vec![("editor.1".to_owned(), 80)]);
        // The next frame has nothing left to request.
        surface.queue_renders_for_frame(80);
        assert!(surface.take_pending_renders().is_empty());
    }

    #[test]
    fn live_component_bound_is_enforced() {
        let mut surface = surface();
        for index in 0..MAX_LIVE_COMPONENTS {
            surface.invalidate(&format!("component.{index}")).unwrap();
        }
        assert_eq!(
            surface.invalidate("component.overflow"),
            Err(ComponentSurfaceError::TooManyLiveComponents)
        );
    }

    #[test]
    fn render_storage_is_bounded_and_change_detecting() {
        let mut surface = surface();
        surface.invalidate("list").unwrap();
        assert!(surface
            .store_render("list", 40, vec!["one".to_owned()])
            .unwrap());
        assert!(!surface
            .store_render("list", 40, vec!["one".to_owned()])
            .unwrap());
        assert!(surface
            .store_render("list", 41, vec!["one".to_owned()])
            .unwrap());
        let oversized: Vec<String> = (0..MAX_RENDER_LINES + 1)
            .map(|index| index.to_string())
            .collect();
        assert!(matches!(
            surface.store_render("list", 40, oversized),
            Err(ComponentSurfaceError::BoundsExceeded(_))
        ));
        assert!(matches!(
            surface.store_render("unclaimed", 40, vec![]),
            Err(ComponentSurfaceError::BoundsExceeded(_))
        ));
    }

    #[test]
    fn widget_set_accepts_lines_and_component_forms_and_clears() {
        let mut surface = surface();
        surface
            .widget_set(
                "status",
                Some(WidgetContent::Lines(vec!["hello".to_owned()])),
                None,
            )
            .unwrap();
        surface
            .widget_set(
                "clock",
                Some(WidgetContent::Component("clock.1".into())),
                None,
            )
            .unwrap();
        // Neither form clears the key, per the wire contract.
        surface.widget_set("status", None, None).unwrap();
        assert_eq!(surface.widget_lines_above_editor().len(), 1);
        assert_eq!(surface.widget_lines_above_editor()[0].0, "clock");
        // Component widgets project from their reserved region once painted.
        surface.invalidate("clock.1").unwrap();
        surface.queue_renders_for_frame(80);
        surface
            .store_render("clock.1", 80, vec!["12:00".to_owned()])
            .unwrap();
        assert_eq!(
            surface.widget_component_lines("clock"),
            Some(&["12:00".to_owned()][..])
        );
        // Oversized lines are rejected with the wire bound.
        let long_line = "x".repeat(MAX_WIDGET_LINE_BYTES + 1);
        assert!(matches!(
            surface.widget_set("big", Some(WidgetContent::Lines(vec![long_line])), None),
            Err(ComponentSurfaceError::BoundsExceeded(_))
        ));
    }

    #[test]
    fn widget_placement_keeps_above_and_below_regions_separate() {
        let mut surface = surface();
        let below = WidgetPlacement::parse("belowEditor").unwrap();
        assert!(WidgetPlacement::parse("invalid").is_none());
        surface
            .widget_set(
                "below",
                Some(WidgetContent::Lines(vec!["below".into()])),
                Some(below),
            )
            .unwrap();
        surface
            .widget_set(
                "above",
                Some(WidgetContent::Lines(vec!["above".into()])),
                None,
            )
            .unwrap();
        assert_eq!(surface.widgets().count(), 2);
        assert_eq!(
            surface.widget_lines_below_editor(),
            vec![("below".into(), vec!["below".into()])]
        );
        assert_eq!(
            surface.widget_lines_above_editor(),
            vec![("above".into(), vec!["above".into()])]
        );
    }

    #[test]
    fn header_footer_editor_slots_route_to_component_regions() {
        let mut surface = surface();
        surface
            .slot_set(ComponentSlot::Header, Some("head.1"))
            .unwrap();
        surface
            .slot_set(ComponentSlot::Footer, Some("foot.1"))
            .unwrap();
        surface
            .slot_set(ComponentSlot::Editor, Some("edit.1"))
            .unwrap();
        assert_eq!(surface.header_component_id(), Some("head.1"));
        assert_eq!(surface.footer_component_id(), Some("foot.1"));
        assert_eq!(surface.editor_component_id(), Some("edit.1"));
        assert_eq!(surface.keyboard_focus_id(), Some("edit.1"));
        surface
            .store_render("head.1", 80, vec!["HEADER".to_owned()])
            .unwrap();
        assert_eq!(
            surface.slot_lines(surface.header_component_id()),
            Some(&["HEADER".to_owned()][..])
        );
        // Absent id restores the built-in surface.
        surface.slot_set(ComponentSlot::Editor, None).unwrap();
        assert_eq!(surface.editor_component_id(), None);
        assert_eq!(surface.keyboard_focus_id(), None);
    }

    #[test]
    fn tools_expanded_and_terminal_title_are_bounded() {
        let mut surface = surface();
        assert!(!surface.tools_expanded());
        surface.set_tools_expanded(true);
        assert!(surface.tools_expanded());
        surface.set_terminal_title(Some("octet")).unwrap();
        assert_eq!(surface.terminal_title(), Some("octet"));
        let oversized = "x".repeat(MAX_TERMINAL_TITLE_BYTES + 1);
        assert!(matches!(
            surface.set_terminal_title(Some(&oversized)),
            Err(ComponentSurfaceError::BoundsExceeded(_))
        ));
        surface.set_terminal_title(None).unwrap();
        assert_eq!(surface.terminal_title(), None);
    }

    #[test]
    fn clear_disposes_every_region() {
        let mut surface = surface();
        surface.invalidate("a").unwrap();
        surface
            .widget_set("w", Some(WidgetContent::Component("a".into())), None)
            .unwrap();
        surface.slot_set(ComponentSlot::Footer, Some("a")).unwrap();
        surface.set_terminal_title(Some("t")).unwrap();
        surface.set_tools_expanded(true);
        surface.clear();
        assert_eq!(surface.component_ids().count(), 0);
        assert_eq!(surface.footer_component_id(), None);
        assert_eq!(surface.terminal_title(), None);
        assert!(!surface.tools_expanded());
        assert!(surface.take_pending_renders().is_empty());
    }

    #[test]
    fn raw_key_data_matches_terminal_spellings() {
        use crossterm::event::{KeyCode, KeyModifiers};
        assert_eq!(raw_key_data(KeyCode::Enter, KeyModifiers::NONE), "\r");
        assert_eq!(raw_key_data(KeyCode::Up, KeyModifiers::NONE), "\x1b[A");
        assert_eq!(raw_key_data(KeyCode::Esc, KeyModifiers::NONE), "\x1b");
        assert_eq!(raw_key_data(KeyCode::Backspace, KeyModifiers::NONE), "\x7f");
        assert_eq!(
            raw_key_data(KeyCode::Char('a'), KeyModifiers::CONTROL),
            "\x01"
        );
        assert_eq!(raw_key_data(KeyCode::Char('a'), KeyModifiers::ALT), "\x1ba");
        assert_eq!(raw_key_data(KeyCode::F(5), KeyModifiers::NONE), "\x1b[5~");
        assert_eq!(
            raw_key_data(KeyCode::PageDown, KeyModifiers::NONE),
            "\x1b[6~"
        );
    }

    #[test]
    fn mouse_wire_event_uses_pi_shape_with_zero_based_local_coordinates() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let event = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 4,
            modifiers: KeyModifiers::SHIFT,
        };
        let wire = mouse_wire_event(&event, (3, 2), (20, 5));
        assert_eq!(wire.kind, "wheel");
        assert_eq!(wire.button, "none");
        assert_eq!(wire.x, 6);
        assert_eq!(wire.y, 1);
        assert_eq!(wire.screen_x, 9);
        assert_eq!(wire.screen_y, 3);
        assert_eq!(wire.width, 20);
        assert_eq!(wire.height, 5);
        assert_eq!(wire.wheel_delta, Some(1));
        assert!(wire.shift);
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"screenX\":9"));
        assert!(json.contains("\"wheelDelta\":1"));
        let press = mouse_wire_event(
            &MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            (0, 0),
            (80, 24),
        );
        assert_eq!(press.kind, "press");
        assert_eq!(press.button, "left");
        assert_eq!(press.x, 0);
        assert_eq!(press.y, 0);
    }
}

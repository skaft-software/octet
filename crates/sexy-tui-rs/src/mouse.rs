//! Component mouse events: normalized events, regions, and capture/focus
//! routing against the frame that was last rendered.
//!
//! This is the Rust port of the component half of Pi's fullscreen mouse model
//! (`packages/tui/src/tui.ts`: `TuiMouseEvent`, `TuiMouseEventResult`,
//! `dispatchMouseEvent`, `retargetMouseEvent`, `TuiMouseDispatchTarget`, and
//! `packages/tui/src/components/mouse-region.ts`). Geometry decisions are made
//! only from a [`crate::layout::LayoutFrame`], i.e. from the rows and boxes the
//! renderer actually produced for the previous frame; nothing here reads the
//! terminal, the clock, or the environment.
//!
//! Product-owned gestures stay product-owned: text selection granularity,
//! scrollbar dragging, overlays, and search all layer on top of this router by
//! inspecting the reported [`MouseRouting`] and the frame boxes. The one product
//! hook this module owns is right-click paste: Pi calls `onRightClickPaste`
//! only after every component declined a right-button press, and so does
//! [`MouseRouter`].

use std::time::{Duration, Instant};

use crate::layout::LayoutFrame;
use crate::tui::Component;

/// Pi's double-click window (`DOUBLE_CLICK_INTERVAL_MS`).
pub const DOUBLE_CLICK_INTERVAL_MS: u64 = 500;

/// Normalized cell-based mouse event type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMouseEventType {
    /// Button pressed.
    Press,
    /// Button released.
    Release,
    /// Pointer moved with no button held.
    Move,
    /// Pointer moved with a button held.
    Drag,
    /// Synthesized consecutive click on release without movement.
    Click,
    /// Wheel scrolled.
    Wheel,
}

/// Mouse button identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMouseButton {
    /// Primary button.
    Left,
    /// Middle button.
    Middle,
    /// Secondary button; the right-click paste fallback listens for this.
    Right,
    /// No button (motion events and wheels).
    None,
}

/// Normalized cell-based mouse event. Coordinates are zero-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TuiMouseEvent {
    /// Event type.
    pub kind: TuiMouseEventType,
    /// Button identity.
    pub button: TuiMouseButton,
    /// Column local to the receiving component.
    pub x: i32,
    /// Row local to the receiving component.
    pub y: i32,
    /// Absolute terminal column.
    pub screen_x: i32,
    /// Absolute terminal row.
    pub screen_y: i32,
    /// Current component width in cells.
    pub width: i32,
    /// Current component height in rows.
    pub height: i32,
    /// Shift modifier.
    pub shift: bool,
    /// Alt modifier. The wheel uses this for Pi's `×5` scroll multiplier.
    pub alt: bool,
    /// Control modifier.
    pub ctrl: bool,
    /// Logical lines; negative scrolls up. Wheel events only.
    pub wheel_delta: Option<i32>,
    /// Consecutive click count when `kind` is [`TuiMouseEventType::Click`].
    pub click_count: Option<u8>,
}

impl TuiMouseEvent {
    /// A screen-space event whose local coordinates start at the pointer.
    pub fn new(
        kind: TuiMouseEventType,
        button: TuiMouseButton,
        screen_x: i32,
        screen_y: i32,
    ) -> Self {
        Self {
            kind,
            button,
            x: screen_x,
            y: screen_y,
            screen_x,
            screen_y,
            width: 0,
            height: 0,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        }
    }

    /// Attach a wheel delta in logical lines.
    pub fn with_wheel_delta(mut self, delta: i32) -> Self {
        self.wheel_delta = Some(delta);
        self
    }

    /// Attach a synthesized click count.
    pub fn with_click_count(mut self, count: u8) -> Self {
        self.click_count = Some(count);
        self
    }

    /// Recreate local coordinates for a retained dispatch target.
    pub fn retarget(&self, target: &TuiMouseDispatchTarget<'_>) -> Self {
        Self {
            x: self.screen_x - target.origin_x,
            y: self.screen_y - target.origin_y,
            width: target.width,
            height: target.height,
            ..*self
        }
    }
}

/// What a component asks the router to do with the event it handled.
///
/// Pi's `TuiMouseEventResult`. An untouched default (`handled == false`,
/// `capture == false`, `focus == false`) means "not mine"; the dispatcher then
/// reports nothing and the next box in the hit path is offered the event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TuiMouseEventResult {
    /// Stop propagation and suppress renderer-level fallback behavior.
    pub handled: bool,
    /// Route subsequent drag/release events to this component.
    pub capture: bool,
    /// Give keyboard focus to this component.
    pub focus: bool,
    /// Explicit render request. `None` uses Pi's per-event default.
    pub render: Option<bool>,
}

impl TuiMouseEventResult {
    /// A plain handled result.
    pub fn handled() -> Self {
        Self {
            handled: true,
            ..Self::default()
        }
    }

    /// Whether the result stops propagation.
    pub fn is_consumed(&self) -> bool {
        self.handled || self.capture || self.focus
    }

    /// Pi's render default: move and release do not repaint; press, click,
    /// drag, and wheel do, and an explicit `render` always wins.
    pub fn should_render(&self, kind: TuiMouseEventType) -> bool {
        self.render.unwrap_or(matches!(
            kind,
            TuiMouseEventType::Press
                | TuiMouseEventType::Click
                | TuiMouseEventType::Drag
                | TuiMouseEventType::Wheel
        ))
    }
}

/// The exact component and geometry a dispatched event landed on.
#[derive(Clone, Copy)]
pub struct TuiMouseDispatchTarget<'a> {
    /// The receiving component.
    pub component: &'a dyn Component,
    /// Absolute column of the component's local origin.
    pub origin_x: i32,
    /// Absolute row of the component's local origin.
    pub origin_y: i32,
    /// Component width in cells.
    pub width: i32,
    /// Component height in rows.
    pub height: i32,
}

impl std::fmt::Debug for TuiMouseDispatchTarget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A component has no printable identity of its own, so debugging names
        // the stable address `component_identity` already uses.
        f.debug_struct("TuiMouseDispatchTarget")
            .field("component", &component_identity(self.component))
            .field("origin_x", &self.origin_x)
            .field("origin_y", &self.origin_y)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// The outcome of dispatching to a concrete component.
#[derive(Clone, Copy)]
pub struct TuiMouseDispatchResult<'a> {
    /// What the component asked for.
    pub result: TuiMouseEventResult,
    /// The component that owns the event.
    pub target: TuiMouseDispatchTarget<'a>,
    /// Keyboard focus owner, which may be a delegating parent container.
    pub focus_target: Option<&'a dyn Component>,
}

impl std::fmt::Debug for TuiMouseDispatchResult<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiMouseDispatchResult")
            .field("result", &self.result)
            .field("target", &self.target)
            .field("focus_target", &self.focus_target.map(component_identity))
            .finish()
    }
}

/// What [`Component::handle_mouse`] returns.
#[derive(Clone, Copy, Debug)]
pub enum MouseOutcome<'a> {
    /// This component owns the event; the dispatcher records it as the target.
    Handled(TuiMouseEventResult),
    /// A wrapped child already owns the event with its own retained target.
    Dispatching(TuiMouseDispatchResult<'a>),
}

/// Dispatch one event to a component and retain the exact target.
pub fn dispatch_mouse_event<'a>(
    component: &'a dyn Component,
    event: &TuiMouseEvent,
) -> Option<TuiMouseDispatchResult<'a>> {
    match component.handle_mouse(event)? {
        MouseOutcome::Dispatching(result) => Some(result),
        MouseOutcome::Handled(result) => {
            if !result.is_consumed() {
                return None;
            }
            Some(TuiMouseDispatchResult {
                result,
                focus_target: result.focus.then_some(component),
                target: TuiMouseDispatchTarget {
                    component,
                    origin_x: event.screen_x - event.x,
                    origin_y: event.screen_y - event.y,
                    width: event.width,
                    height: event.height,
                },
            })
        }
    }
}

/// Handler added to an existing component without changing its rendering.
pub type MouseRegionHandler = Box<dyn Fn(&TuiMouseEvent) -> Option<TuiMouseEventResult>>;

/// Pi's `MouseRegion`: forwards to the child first, then to its own handler.
pub struct MouseRegion {
    child: Box<dyn Component>,
    on_mouse: MouseRegionHandler,
}

impl MouseRegion {
    /// Wrap `child` with an additional mouse handler.
    pub fn new(child: Box<dyn Component>, on_mouse: MouseRegionHandler) -> Self {
        Self { child, on_mouse }
    }

    /// The wrapped component.
    pub fn child(&self) -> &dyn Component {
        self.child.as_ref()
    }
}

impl Component for MouseRegion {
    fn render(&self, width: u16) -> Vec<String> {
        self.child.render(width)
    }

    fn render_update(&self, width: u16) -> Option<crate::tui::FrameUpdate> {
        self.child.render_update(width)
    }

    fn render_update_with_cursor(
        &self,
        width: u16,
        cursor: Option<crate::tui::CommitCursor>,
    ) -> Option<crate::tui::FrameUpdate> {
        self.child.render_update_with_cursor(width, cursor)
    }

    fn handle_input(&mut self, data: &str) {
        self.child.handle_input(data);
    }

    fn handle_paste(&mut self, data: &str) {
        self.child.handle_paste(data);
    }

    fn wants_key_release(&self) -> bool {
        self.child.wants_key_release()
    }

    fn invalidate(&mut self) {
        self.child.invalidate();
    }

    /// A region is a layout *leaf*, exactly like Pi's `MouseRegion`: it renders
    /// its child's lines but does not expose the child's layout node, so the
    /// box the hit path targets is the region itself and its handler keeps
    /// ownership of the event.
    fn handle_mouse<'a>(&'a self, event: &TuiMouseEvent) -> Option<MouseOutcome<'a>> {
        if let Some(result) = dispatch_mouse_event(self.child.as_ref(), event) {
            return Some(MouseOutcome::Dispatching(result));
        }
        (self.on_mouse)(event).map(MouseOutcome::Handled)
    }
}

/// Stable identity of a component inside one retained tree.
///
/// Boxed components never move, so the data address is stable for the life of
/// the tree; the router stores identities rather than borrows so it can outlive
/// any single rendered frame.
pub fn component_identity(component: &dyn Component) -> usize {
    component as *const dyn Component as *const () as usize
}

/// A retained capture/press target: identity plus the geometry it was entered
/// with, exactly like Pi's `TuiMouseDispatchTarget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseCapture {
    /// [`component_identity`] of the owning component.
    pub identity: usize,
    /// Absolute column of the component's local origin.
    pub origin_x: i32,
    /// Absolute row of the component's local origin.
    pub origin_y: i32,
    /// Component width in cells at capture time.
    pub width: i32,
    /// Component height in rows at capture time.
    pub height: i32,
}

/// One router decision, with the facts a product needs to react.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MouseRouting {
    /// A component, the wheel fallback, or the paste hook consumed the event.
    pub handled: bool,
    /// The product should repaint.
    pub render: bool,
    /// Right-click paste was accepted by the product hook.
    pub paste_requested: bool,
    /// OSC 8 target under a press that may still become a drag.
    pub pressed_link: Option<String>,
    /// OSC 8 target activated by a release without movement.
    pub activated_link: Option<String>,
    /// Focus owner after this event, when it changed.
    pub focus_target: Option<usize>,
    /// Unconsumed wheel delta after every scroll view declined it. `None` for
    /// non-wheel events.
    pub wheel_remaining: Option<i32>,
}

/// The OSC 8 hyperlink target shown at one cell of the retained frame.
pub fn link_at(frame: &LayoutFrame<'_>, x: i32, y: i32) -> Option<String> {
    if x < 0 || y < 0 {
        return None;
    }
    let line = frame.lines.get(usize::try_from(y).ok()?)?;
    crate::utils::hyperlink_at_column(line, usize::try_from(x).ok()?)
}

/// Stateful mouse router over the last rendered layout frame.
///
/// It owns everything Pi's `TuiAltScreen` owns between frames: capture, press
/// tracking, click counting, focus, hover, and the right-click-paste hook. The
/// application still owns selection, overlays, search, and scrollbar dragging.
pub struct MouseRouter {
    capture: Option<MouseCapture>,
    press_target: Option<MouseCapture>,
    press_point: Option<(i32, i32)>,
    press_moved: bool,
    last_click: Option<(usize, Instant, i32, i32, u8)>,
    focused: Option<usize>,
    hover: Option<usize>,
    double_click_interval: Duration,
    on_right_click_paste: Option<Box<dyn FnMut() -> bool>>,
}

impl Default for MouseRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseRouter {
    /// A router with Pi's default double-click window.
    pub fn new() -> Self {
        Self {
            capture: None,
            press_target: None,
            press_point: None,
            press_moved: false,
            last_click: None,
            focused: None,
            hover: None,
            double_click_interval: Duration::from_millis(DOUBLE_CLICK_INTERVAL_MS),
            on_right_click_paste: None,
        }
    }

    /// Override the double-click window (tests and fast pointer hardware).
    pub fn with_double_click_interval(mut self, interval: Duration) -> Self {
        self.double_click_interval = interval;
        self
    }

    /// Install the product's right-click paste hook. Pi invokes it only after
    /// every component declined a right-button press; the platform and
    /// terminal gate (Windows, non-VS-Code) stays the product's decision.
    pub fn set_right_click_paste(&mut self, hook: Box<dyn FnMut() -> bool>) {
        self.on_right_click_paste = Some(hook);
    }

    /// The component identity that currently owns keyboard focus, if any.
    pub fn focused(&self) -> Option<usize> {
        self.focused
    }

    /// Focus a component directly (for example after a keyboard transition).
    pub fn set_focus(&mut self, identity: Option<usize>) {
        self.focused = identity;
    }

    /// The deepest component identity under the last move event.
    pub fn hovered(&self) -> Option<usize> {
        self.hover
    }

    /// The retained capture target, if a component asked for one.
    pub fn capture(&self) -> Option<MouseCapture> {
        self.capture
    }

    /// Drop the transient press/capture gesture.
    ///
    /// Pi calls this after a completed press/release pair and when the alt
    /// screen closes. The consecutive-click history deliberately survives: Pi's
    /// `lastComponentClick` is what makes the *next* stationary click a
    /// double-click, and only pointer movement (or an explicit interaction
    /// reset) forgets it.
    pub fn clear_gesture(&mut self) {
        self.capture = None;
        self.press_target = None;
        self.press_point = None;
        self.press_moved = false;
    }

    /// Forget the consecutive-click history. Pi drops it on movement (already
    /// handled here) and when keyboard focus is lost; a product that resets
    /// interaction state calls this explicitly.
    pub fn forget_click_history(&mut self) {
        self.last_click = None;
    }

    /// Route one normalized event against the last rendered frame.
    pub fn handle(
        &mut self,
        frame: &LayoutFrame<'_>,
        event: &TuiMouseEvent,
        now: Instant,
    ) -> MouseRouting {
        let mut routing = MouseRouting::default();
        if self.capture.is_some() || self.press_target.is_some() {
            let target = self
                .capture
                .or(self.press_target)
                .expect("one gesture target is present");
            if self
                .press_point
                .is_some_and(|point| point != (event.screen_x, event.screen_y))
            {
                self.press_moved = true;
                self.last_click = None;
            }
            routing.handled = true;
            if let Some(dispatch) = dispatch_to_retained(frame, target, event) {
                let render = self.apply(&mut routing, event, &dispatch);
                routing.render = render;
            }
            if event.kind == TuiMouseEventType::Release {
                if !self.press_moved && self.press_point == Some((event.screen_x, event.screen_y)) {
                    let count = self.click_count(target.identity, event, now);
                    let click = TuiMouseEvent {
                        kind: TuiMouseEventType::Click,
                        click_count: Some(count),
                        ..*event
                    };
                    if let Some(dispatch) = dispatch_to_retained(frame, target, &click) {
                        let render = self.apply(&mut routing, &click, &dispatch);
                        routing.render |= render;
                    }
                }
                self.clear_gesture();
            }
            return routing;
        }

        self.hover = frame.deepest_component_identity(event.screen_x, event.screen_y);
        if event.kind == TuiMouseEventType::Press {
            // Pi records the target under the press pointer whether or not a
            // component claimed it, and opens it on a release that never became
            // a drag.
            routing.pressed_link = link_at(frame, event.screen_x, event.screen_y);
        }
        if let Some(dispatch) = frame.dispatch_at(event) {
            routing.handled = true;
            let render = self.apply(&mut routing, event, &dispatch);
            routing.render = render;
            if event.kind == TuiMouseEventType::Press {
                self.press_target = Some(capture_of(&dispatch));
                self.press_point = Some((event.screen_x, event.screen_y));
                self.press_moved = false;
            }
            return routing;
        }

        if event.kind == TuiMouseEventType::Press && event.button == TuiMouseButton::Right {
            if let Some(hook) = self.on_right_click_paste.as_mut() {
                if hook() {
                    routing.handled = true;
                    routing.paste_requested = true;
                    return routing;
                }
            }
        }

        if matches!(
            event.kind,
            TuiMouseEventType::Click | TuiMouseEventType::Release
        ) {
            routing.activated_link = link_at(frame, event.screen_x, event.screen_y);
            // A click that lands on a validated target is an action even though
            // no component claimed it; the product owns what activation does.
            routing.handled |= routing.activated_link.is_some();
        }
        if event.kind == TuiMouseEventType::Wheel {
            let delta = event.wheel_delta.unwrap_or(0);
            routing.handled = true;
            routing.wheel_remaining = Some(crate::layout::route_wheel_delta(
                frame,
                event.screen_x,
                event.screen_y,
                delta,
            ));
        }
        routing
    }

    fn apply(
        &mut self,
        routing: &mut MouseRouting,
        event: &TuiMouseEvent,
        dispatch: &TuiMouseDispatchResult<'_>,
    ) -> bool {
        let focus_target = dispatch.focus_target.map(component_identity);
        let focus_changed = dispatch.result.focus && self.focused != focus_target;
        if dispatch.result.focus {
            self.focused = focus_target;
            routing.focus_target = focus_target;
        }
        if dispatch.result.capture {
            self.capture = Some(capture_of(dispatch));
        }
        dispatch.result.render.unwrap_or(
            focus_changed
                || matches!(
                    event.kind,
                    TuiMouseEventType::Press
                        | TuiMouseEventType::Click
                        | TuiMouseEventType::Drag
                        | TuiMouseEventType::Wheel
                ),
        )
    }

    fn click_count(&mut self, identity: usize, event: &TuiMouseEvent, now: Instant) -> u8 {
        let previous = self.last_click;
        let count = match previous {
            Some((component, timestamp, x, y, count))
                if component == identity
                    && x == event.screen_x
                    && y == event.screen_y
                    && now.saturating_duration_since(timestamp) <= self.double_click_interval =>
            {
                (count % 3) + 1
            }
            _ => 1,
        };
        self.last_click = Some((identity, now, event.screen_x, event.screen_y, count));
        count
    }
}

fn capture_of(dispatch: &TuiMouseDispatchResult<'_>) -> MouseCapture {
    MouseCapture {
        identity: component_identity(dispatch.target.component),
        origin_x: dispatch.target.origin_x,
        origin_y: dispatch.target.origin_y,
        width: dispatch.target.width,
        height: dispatch.target.height,
    }
}

/// Re-dispatch to a retained target, resolving the live component by identity.
fn dispatch_to_retained<'a>(
    frame: &LayoutFrame<'a>,
    capture: MouseCapture,
    event: &TuiMouseEvent,
) -> Option<TuiMouseDispatchResult<'a>> {
    let component = frame.component_by_identity(capture.identity)?;
    let target = TuiMouseDispatchTarget {
        component,
        origin_x: capture.origin_x,
        origin_y: capture.origin_y,
        width: capture.width,
        height: capture.height,
    };
    dispatch_mouse_event(component, &event.retarget(&target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{render_layout_frame, LayoutBox, LayoutRect, VStack};
    use crate::tui::FrameUpdate;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    struct Lines {
        lines: Vec<String>,
    }

    impl Lines {
        fn new(lines: &[&str]) -> Self {
            Self {
                lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            }
        }
    }

    impl Component for Lines {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }

        fn invalidate(&mut self) {}
    }

    fn test_frame(root: &dyn Component, width: u16, height: u16) -> LayoutFrame<'_> {
        render_layout_frame(root, width, height, Rc::new(|| {}))
    }

    fn press(x: i32, y: i32) -> TuiMouseEvent {
        TuiMouseEvent::new(TuiMouseEventType::Press, TuiMouseButton::Left, x, y)
    }

    fn drag(x: i32, y: i32) -> TuiMouseEvent {
        TuiMouseEvent::new(TuiMouseEventType::Drag, TuiMouseButton::Left, x, y)
    }

    fn release(x: i32, y: i32) -> TuiMouseEvent {
        TuiMouseEvent::new(TuiMouseEventType::Release, TuiMouseButton::Left, x, y)
    }

    fn moved(x: i32, y: i32) -> TuiMouseEvent {
        TuiMouseEvent::new(TuiMouseEventType::Move, TuiMouseButton::None, x, y)
    }

    #[test]
    fn mouse_region_forwards_to_the_child_before_its_own_handler() {
        let seen: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let child_seen = seen.clone();
        let child = MouseRegion::new(
            Box::new(Lines::new(&["child"])),
            Box::new(move |event| {
                child_seen.borrow_mut().push(event.x);
                Some(TuiMouseEventResult::handled())
            }),
        );
        let outer_seen = seen.clone();
        let region = MouseRegion::new(
            Box::new(child),
            Box::new(move |event| {
                outer_seen.borrow_mut().push(-event.x);
                Some(TuiMouseEventResult::handled())
            }),
        );
        let component: &dyn Component = &region;
        let dispatch = dispatch_mouse_event(component, &press(3, 0)).expect("handled");
        assert_eq!(
            dispatch.target.origin_x, 0,
            "the innermost region's local origin is the pointer column it saw"
        );
        assert_eq!(*seen.borrow(), vec![3], "the child owns the event first");
    }

    #[test]
    fn mouse_region_falls_back_to_its_own_handler_when_the_child_declines() {
        let seen: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let outer_seen = seen.clone();
        let region = MouseRegion::new(
            Box::new(Lines::new(&["child"])),
            Box::new(move |_event| {
                *outer_seen.borrow_mut() += 1;
                Some(TuiMouseEventResult {
                    handled: true,
                    capture: true,
                    ..TuiMouseEventResult::default()
                })
            }),
        );
        let component: &dyn Component = &region;
        let dispatch = dispatch_mouse_event(component, &press(1, 1)).expect("handled");
        assert!(dispatch.result.capture);
        assert_eq!(
            dispatch.target.component as *const dyn Component as *const (),
            { component as *const dyn Component as *const () }
        );
        assert_eq!(*seen.borrow(), 1);
    }

    #[test]
    fn default_results_do_not_consume_an_event() {
        let region = MouseRegion::new(
            Box::new(Lines::new(&["child"])),
            Box::new(|_event| Some(TuiMouseEventResult::default())),
        );
        let component: &dyn Component = &region;
        assert!(dispatch_mouse_event(component, &press(0, 0)).is_none());
    }

    #[test]
    fn layout_dispatch_targets_the_deepest_component_and_retargets_coordinates() {
        let seen = Rc::new(RefCell::new(Vec::<(i32, i32, i32, i32)>::new()));
        let inner_seen = seen.clone();
        let inner = MouseRegion::new(
            Box::new(Lines::new(&["inner"])),
            Box::new(move |event| {
                inner_seen
                    .borrow_mut()
                    .push((event.x, event.y, event.width, event.height));
                Some(TuiMouseEventResult::handled())
            }),
        );
        let mut outer = VStack::new();
        outer.push(Box::new(Lines::new(&["first", "second"])));
        outer.push(Box::new(inner));
        let frame = test_frame(&outer, 40, 10);
        // The two-row first child puts the region's one-row box at y = 2, so
        // row 2 is the row that belongs to the region.
        let mut router = MouseRouter::new();
        let routing = router.handle(&frame, &press(3, 2), Instant::now());
        assert!(routing.handled);
        assert_eq!(
            *seen.borrow(),
            vec![(3, 0, 40, 1)],
            "the child sees local coordinates inside its own box"
        );
    }

    #[test]
    fn capture_routes_drag_and_release_to_the_pressed_component() {
        let seen: Rc<RefCell<Vec<(TuiMouseEventType, i32, i32)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let region_seen = seen.clone();
        let region = MouseRegion::new(
            Box::new(Lines::new(&["leaf"])),
            Box::new(move |event| {
                region_seen
                    .borrow_mut()
                    .push((event.kind, event.x, event.y));
                Some(TuiMouseEventResult {
                    handled: true,
                    capture: true,
                    ..TuiMouseEventResult::default()
                })
            }),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(Lines::new(&["above"])));
        stack.push(Box::new(region));
        let frame = test_frame(&stack, 20, 8);
        let mut router = MouseRouter::new();
        let now = Instant::now();
        assert!(router.handle(&frame, &press(2, 1), now).handled);
        assert!(router.capture().is_some());
        let routing = router.handle(&frame, &drag(2, 6), now);
        assert!(routing.handled, "captured drags stay routed");
        let routing = router.handle(&frame, &release(2, 6), now);
        assert!(routing.handled);
        assert!(
            seen.borrow()
                .iter()
                .any(|(kind, x, y)| *kind == TuiMouseEventType::Drag && *x == 2 && *y == 5),
            "the drag was retargeted into the captured component: {:?}",
            seen.borrow()
        );
        assert!(router.capture().is_none(), "release clears the gesture");
    }

    #[test]
    fn click_is_synthesized_only_without_movement_and_counts_double_clicks() {
        let clicks: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let click_seen = clicks.clone();
        let region = MouseRegion::new(
            Box::new(Lines::new(&["leaf"])),
            Box::new(move |event| {
                if event.kind == TuiMouseEventType::Click {
                    click_seen.borrow_mut().push(event.click_count.unwrap_or(0));
                }
                Some(TuiMouseEventResult {
                    handled: true,
                    capture: true,
                    ..TuiMouseEventResult::default()
                })
            }),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(region));
        let frame = test_frame(&stack, 20, 6);
        let mut router = MouseRouter::new();
        let now = Instant::now();
        let _ = router.handle(&frame, &press(1, 0), now);
        let _ = router.handle(&frame, &release(1, 0), now);
        let _ = router.handle(&frame, &press(1, 0), now + Duration::from_millis(120));
        let _ = router.handle(&frame, &release(1, 0), now + Duration::from_millis(140));
        assert_eq!(*clicks.borrow(), vec![1, 2]);

        // A moved press/release pair is a selection gesture, not a click.
        let _ = router.handle(&frame, &press(1, 0), now + Duration::from_millis(2_000));
        let _ = router.handle(&frame, &drag(1, 3), now + Duration::from_millis(2_010));
        let _ = router.handle(&frame, &release(1, 3), now + Duration::from_millis(2_020));
        assert_eq!(
            *clicks.borrow(),
            vec![1, 2],
            "dragged releases are not clicks"
        );

        // A click after the window restarts at one.
        let _ = router.handle(&frame, &press(1, 0), now + Duration::from_millis(9_000));
        let _ = router.handle(&frame, &release(1, 0), now + Duration::from_millis(9_010));
        assert_eq!(*clicks.borrow(), vec![1, 2, 1]);
    }

    #[test]
    fn focus_and_hover_follow_the_last_frame() {
        let region = MouseRegion::new(
            Box::new(Lines::new(&["focusable"])),
            Box::new(|_event| {
                Some(TuiMouseEventResult {
                    focus: true,
                    ..TuiMouseEventResult::default()
                })
            }),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(Lines::new(&["above"])));
        stack.push(Box::new(region));
        let frame = test_frame(&stack, 20, 6);
        let identity = frame
            .deepest_component_identity(4, 1)
            .expect("the wrapped leaf is under the pointer");
        let mut router = MouseRouter::new();
        let routing = router.handle(&frame, &press(4, 1), Instant::now());
        assert!(routing.handled);
        assert_eq!(routing.focus_target, Some(identity));
        assert_eq!(router.focused(), Some(identity));

        let routing = router.handle(&frame, &moved(4, 1), Instant::now());
        assert_eq!(router.hovered(), Some(identity));
        assert!(!routing.render, "moves do not repaint by default");
    }

    #[test]
    fn right_click_paste_falls_back_to_the_product_hook() {
        let calls: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let hook_calls = calls.clone();
        let mut router = MouseRouter::new();
        router.set_right_click_paste(Box::new(move || {
            *hook_calls.borrow_mut() += 1;
            true
        }));
        let stack = VStack::new();
        let frame = test_frame(&stack, 20, 6);
        let press = TuiMouseEvent::new(TuiMouseEventType::Press, TuiMouseButton::Right, 1, 1);
        let routing = router.handle(&frame, &press, Instant::now());
        assert!(routing.handled);
        assert!(routing.paste_requested);
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn right_click_paste_is_not_invoked_when_a_component_handles_the_press() {
        let calls: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let hook_calls = calls.clone();
        let mut router = MouseRouter::new();
        router.set_right_click_paste(Box::new(move || {
            *hook_calls.borrow_mut() += 1;
            true
        }));
        let region = MouseRegion::new(
            Box::new(Lines::new(&["leaf"])),
            Box::new(|_event| Some(TuiMouseEventResult::handled())),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(region));
        let frame = test_frame(&stack, 20, 6);
        let press = TuiMouseEvent::new(TuiMouseEventType::Press, TuiMouseButton::Right, 1, 0);
        let routing = router.handle(&frame, &press, Instant::now());
        assert!(routing.handled);
        assert!(!routing.paste_requested);
        assert_eq!(*calls.borrow(), 0);
    }

    #[test]
    fn click_activation_reports_the_osc8_link_under_the_pointer() {
        // No component claims the press, so the product-level link fallback is
        // what a click on the label resolves.
        let mut stack = VStack::new();
        stack.push(Box::new(Lines::new(&[
            "\x1b]8;;https://example.test/docs\x07docs\x1b]8;;\x07 tail",
        ])));
        let frame = test_frame(&stack, 40, 4);
        let mut router = MouseRouter::new();
        let now = Instant::now();
        let routing = router.handle(&frame, &press(1, 0), now);
        assert_eq!(
            routing.pressed_link.as_deref(),
            Some("https://example.test/docs")
        );
        let routing = router.handle(&frame, &release(1, 0), now);
        assert_eq!(
            routing.activated_link.as_deref(),
            Some("https://example.test/docs")
        );
        assert!(routing.handled, "an activated link is an action");
        let routing = router.handle(&frame, &release(20, 0), now);
        assert_eq!(routing.activated_link, None);
    }

    #[test]
    fn wheel_events_chain_through_scroll_views_and_report_the_remainder() {
        use crate::layout::{Overscroll, ScrollView, ScrollViewOptions};
        let body: Vec<String> = (0..40).map(|index| format!("row {index}")).collect();
        let inner = ScrollView::new(
            Box::new(Lines { lines: body }),
            ScrollViewOptions::new().with_overscroll(Overscroll::Contain),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(inner));
        let frame = test_frame(&stack, 20, 6);
        let views = frame.scroll_views_at(1, 3);
        assert_eq!(views.len(), 1, "the scroll viewport contains the pointer");
        assert_eq!(views[0].viewport_height(), 6);
        let mut router = MouseRouter::new();
        // The view starts at the content top, so scrolling down (a positive
        // delta) is what it can consume; an up-wheel would be clamped and the
        // remainder reported, which is the next test's subject.
        let wheel = TuiMouseEvent::new(TuiMouseEventType::Wheel, TuiMouseButton::None, 1, 3)
            .with_wheel_delta(2);
        let routing = router.handle(&frame, &wheel, Instant::now());
        assert_eq!(routing.wheel_remaining, Some(0));
        assert_eq!(
            views[0].scroll_top(),
            2,
            "the wheel scrolled the view itself"
        );
    }

    #[test]
    fn frame_dispatch_and_link_lookup_use_the_last_rendered_rows() {
        let mut stack = VStack::new();
        stack.push(Box::new(Lines::new(&["plain"])));
        stack.push(Box::new(Lines::new(&[
            "\x1b]8;;https://a.test\x07a\x1b]8;;\x07",
        ])));
        let frame = test_frame(&stack, 20, 4);
        assert_eq!(frame.dispatch_at(&press(1, 1)).map(|_| ()), None);
        assert_eq!(
            link_at(&frame, 0, 1).as_deref(),
            Some("https://a.test"),
            "row zero of the second child is frame row one"
        );
        assert_eq!(link_at(&frame, 0, 0), None);
    }

    #[test]
    fn gesture_reset_forgets_capture_and_press_state() {
        let region = MouseRegion::new(
            Box::new(Lines::new(&["leaf"])),
            Box::new(|_event| {
                Some(TuiMouseEventResult {
                    handled: true,
                    capture: true,
                    ..TuiMouseEventResult::default()
                })
            }),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(region));
        let frame = test_frame(&stack, 20, 6);
        let mut router = MouseRouter::new();
        let _ = router.handle(&frame, &press(1, 0), Instant::now());
        assert!(router.capture().is_some());
        router.clear_gesture();
        assert!(router.capture().is_none());
    }

    #[test]
    fn a_layout_frame_can_construct_boxes_without_a_root_component() {
        // Sanity check that the public box fields stay assignable for products
        // that compose frames from a retained document.
        let box_: LayoutBox<'_> = LayoutBox {
            component: &Lines::new(&["x"]),
            rect: LayoutRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            clip: LayoutRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            children: Vec::new(),
            lines: None,
            line_offset: 0,
            scroll_view: None,
            scroll_content_lines: None,
        };
        assert_eq!(box_.rect.height, 1);
        let _ = FrameUpdate {
            stable_prefix: 0,
            replacement: Vec::new(),
            pinned: None,
            resize_replay: None,
            reanchor_viewport: false,
            rebuild_scrollback: false,
        };
    }
}

//! Constrained component layout: root/VStack/HStack/ScrollView sizing,
//! visibility, nested scrolling, and last-frame hit-testing.
//!
//! Rust port of Pi's layout core (`packages/tui/src/layout.ts`,
//! `packages/tui/src/layout-node.ts`, `packages/tui/src/components/stack.ts`,
//! and `packages/tui/src/components/scroll-view.ts`). A layout root is any
//! component whose [`Component::layout_node`] reports a constrained node; the
//! frame is resolved against the previous viewport and returns both the painted
//! rows ([`LayoutFrame::lines`]) and the box tree the renderer actually used,
//! so every later geometry decision — wheel routing, mouse dispatch, scrollbar
//! hit-testing — is made from the last rendered frame rather than a re-layout.
//!
//! Documented divergences from the reference:
//!
//! - Kitty image cropping and placement bookkeeping are not part of this port;
//!   image lines are handed through the compositor unchanged. Products that
//!   paint indexed images keep their existing renderer for that surface.
//! - Style merging across composite boundaries uses [`crate::utils::slice_by_column`]
//!   rather than Pi's cell-by-cell ANSI state merge. Rendered rows are expected
//!   to end with a line reset (the Pi renderer adds one), so a composited row
//!   never inherits a neighbouring style.
//! - The `auto` transient scrollbar is driven by the product event loop:
//!   [`ScrollLayoutState::expire_transient_scrollbar`] is the deadline owner
//!   and no background timer is created.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::mouse::{component_identity, dispatch_mouse_event, TuiMouseDispatchResult, TuiMouseEvent};
use crate::tui::Component;
use crate::utils::{slice_by_column, visible_width};

/// Pi's `ALT_WHEEL_SCROLL_MULTIPLIER`.
pub const ALT_WHEEL_SCROLL_MULTIPLIER: i32 = 5;

/// Logical lines a wheel event should scroll for its raw SGR button code.
///
/// Bit 3 (value 8) is the Alt modifier; Alt-wheel scrolls `×5`.
pub fn wheel_scroll_lines(button: u8, wheel_scroll_lines: i32) -> i32 {
    if button & 8 != 0 {
        wheel_scroll_lines.saturating_mul(ALT_WHEEL_SCROLL_MULTIPLIER)
    } else {
        wheel_scroll_lines
    }
}

/// An axis-aligned rectangle in cell coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutRect {
    /// Left column.
    pub x: i32,
    /// Top row.
    pub y: i32,
    /// Width in cells.
    pub width: i32,
    /// Height in rows.
    pub height: i32,
}

impl LayoutRect {
    /// Whether the zero-based cell lies inside the half-open rectangle.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// The viewport a `visible` callback is evaluated against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutViewport {
    /// Viewport width in cells.
    pub width: i32,
    /// Viewport height in rows.
    pub height: i32,
}

/// Cross-axis alignment for a horizontal stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StackAlign {
    /// Children fill the allocated height.
    #[default]
    Stretch,
    /// Children keep their intrinsic height at the top.
    Start,
    /// Children are centred.
    Center,
    /// Children sit at the bottom.
    End,
}

/// A child's main-axis basis: intrinsic content size or a fixed number of rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StackBasis {
    /// Measure the child's own render.
    #[default]
    Auto,
    /// Use the given size regardless of the child's render.
    Fixed(i32),
}

/// Optional per-child sizing, matching Pi's `StackEntryOptions`.
#[derive(Default)]
pub struct StackEntryOptions {
    /// Main-axis basis, `None` means automatic measurement.
    pub basis: Option<StackBasis>,
    /// Growth weight when the stack has spare space.
    pub grow: Option<i32>,
    /// Shrink weight when the stack is over-subscribed.
    pub shrink: Option<i32>,
    /// Minimum main-axis size.
    pub min_size: Option<i32>,
    /// Maximum main-axis size.
    pub max_size: Option<i32>,
    /// Hide the child when the callback returns `false` for the viewport.
    pub visible: Option<Box<dyn Fn(LayoutViewport) -> bool>>,
}

impl StackEntryOptions {
    /// Empty options (all defaults).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the main-axis basis.
    pub fn with_basis(mut self, basis: StackBasis) -> Self {
        self.basis = Some(basis);
        self
    }

    /// Set the growth weight.
    pub fn with_grow(mut self, grow: i32) -> Self {
        self.grow = Some(grow);
        self
    }

    /// Set the shrink weight.
    pub fn with_shrink(mut self, shrink: i32) -> Self {
        self.shrink = Some(shrink);
        self
    }

    /// Set the minimum size.
    pub fn with_min_size(mut self, min_size: i32) -> Self {
        self.min_size = Some(min_size);
        self
    }

    /// Set the maximum size.
    pub fn with_max_size(mut self, max_size: i32) -> Self {
        self.max_size = Some(max_size);
        self
    }

    /// Set the visibility callback (evaluated against the resolved viewport).
    pub fn with_visible(mut self, visible: impl Fn(LayoutViewport) -> bool + 'static) -> Self {
        self.visible = Some(Box::new(visible));
        self
    }
}

/// One resolved stack entry.
#[derive(Clone, Copy)]
pub struct StackLayoutEntry<'a> {
    /// The child component.
    pub component: &'a dyn Component,
    /// Main-axis basis.
    pub basis: StackBasis,
    /// Growth weight.
    pub grow: i32,
    /// Shrink weight.
    pub shrink: i32,
    /// Minimum main-axis size.
    pub min_size: i32,
    /// Maximum main-axis size.
    pub max_size: i32,
    /// Visibility callback.
    pub visible: Option<&'a dyn Fn(LayoutViewport) -> bool>,
}

impl StackLayoutEntry<'_> {
    /// Whether the entry participates in this viewport.
    pub fn is_visible(&self, viewport: LayoutViewport) -> bool {
        self.visible.map_or(true, |visible| visible(viewport))
    }
}

/// A resolved vertical or horizontal stack.
pub struct StackLayoutNode<'a> {
    /// Visible and hidden entries in declaration order.
    pub entries: Vec<StackLayoutEntry<'a>>,
    /// Gap between adjacent children in rows (vertical) or cells (horizontal).
    pub gap: i32,
    /// Cross-axis alignment.
    pub align: StackAlign,
}

/// A resolved scroll view.
pub struct ScrollLayoutNode<'a> {
    /// The scrolled content component.
    pub component: &'a dyn Component,
    /// The retained scroll state.
    pub state: &'a dyn ScrollLayoutState,
}

/// A component's constrained layout description.
pub enum LayoutNode<'a> {
    /// Vertical stack.
    VStack(StackLayoutNode<'a>),
    /// Horizontal stack.
    HStack(StackLayoutNode<'a>),
    /// Scroll view.
    Scroll(ScrollLayoutNode<'a>),
}

/// What happens to a wheel delta a scroll view could not consume.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overscroll {
    /// The remainder passes to the next enclosing scroll view, and - if no
    /// enclosing view consumed it either - the layout's primary scroll view
    /// absorbs it. A fully unconsumed remainder is returned to the caller.
    #[default]
    Chain,
    /// This boundary owns the gesture. Nothing outside it (not the enclosing
    /// views and not the primary view) may consume the remainder, which is
    /// returned to the caller untouched.
    Contain,
}

/// Transient scrollbar visibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scrollbar {
    /// Never painted.
    #[default]
    Hidden,
    /// Painted during/after activity and hidden when the deadline expires.
    Auto,
    /// Always painted while the viewport has rows.
    Always,
}

/// Scroll view construction options.
pub struct ScrollViewOptions {
    /// Follow the content end until the reader scrolls away.
    pub follow_end: bool,
    /// Prefer this view as the layout's primary scroll view.
    pub primary: bool,
    /// Nested-scroll behavior.
    pub overscroll: Overscroll,
    /// Scrollbar visibility mode.
    pub scrollbar: Scrollbar,
    /// Transient scrollbar hide delay.
    pub scrollbar_hide_delay: Duration,
    /// Track cell renderer.
    pub scrollbar_track_style: Option<Box<dyn Fn(&str) -> String>>,
    /// Thumb cell renderer.
    pub scrollbar_thumb_style: Option<Box<dyn Fn(&str) -> String>>,
}

impl Default for ScrollViewOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollViewOptions {
    /// Pi's defaults: no follow, chained overscroll, hidden scrollbar.
    pub fn new() -> Self {
        Self {
            follow_end: false,
            primary: false,
            overscroll: Overscroll::Chain,
            scrollbar: Scrollbar::Hidden,
            scrollbar_hide_delay: Duration::from_millis(1000),
            scrollbar_track_style: None,
            scrollbar_thumb_style: None,
        }
    }

    /// Follow the content end by default.
    pub fn with_follow_end(mut self, follow_end: bool) -> Self {
        self.follow_end = follow_end;
        self
    }

    /// Mark this view as the preferred primary scroll view.
    pub fn with_primary(mut self, primary: bool) -> Self {
        self.primary = primary;
        self
    }

    /// Set nested-scroll behavior.
    pub fn with_overscroll(mut self, overscroll: Overscroll) -> Self {
        self.overscroll = overscroll;
        self
    }

    /// Set scrollbar visibility.
    pub fn with_scrollbar(mut self, scrollbar: Scrollbar) -> Self {
        self.scrollbar = scrollbar;
        self
    }

    /// Set the transient scrollbar hide delay.
    pub fn with_scrollbar_hide_delay(mut self, delay: Duration) -> Self {
        self.scrollbar_hide_delay = delay;
        self
    }
}

/// Retained scroll state, exposed to the layout engine.
///
/// Every method takes `&self`: the state lives behind interior mutability so a
/// layout pass can resolve geometry while the owning component still renders.
pub trait ScrollLayoutState {
    /// Current scroll offset in rows from the content start.
    fn scroll_top(&self) -> i32;
    /// Whether the view is pinned to the content end.
    fn is_following_end(&self) -> bool;
    /// Content height in rows.
    fn content_height(&self) -> i32;
    /// Viewport height in rows.
    fn viewport_height(&self) -> i32;
    /// Whether this is the layout's primary scroll view.
    fn is_primary(&self) -> bool;
    /// Nested-scroll behavior.
    fn overscroll(&self) -> Overscroll;
    /// Scrollbar visibility mode.
    fn scrollbar(&self) -> Scrollbar;
    /// Whether the scrollbar should be painted in this frame.
    fn is_scrollbar_visible(&self) -> bool;
    /// Whether the scrollbar is drawn in its active (hover/drag) state.
    fn is_scrollbar_active(&self) -> bool;
    /// Content width after reserving the scrollbar column when always-on.
    fn get_content_width(&self, width: i32) -> i32;
    /// Record the resolved layout geometry.
    fn update_layout(&self, content_height: i32, viewport_height: i32, request_render: Rc<dyn Fn()>);
    /// Jump to a clamped offset.
    fn scroll_to(&self, scroll_top: i32, disable_follow: bool);
    /// Scroll by logical lines, returning the unconsumed remainder.
    fn scroll_by(&self, lines: i32) -> i32;
    /// Jump to the content start.
    fn scroll_to_start(&self);
    /// Jump to the content end.
    fn scroll_to_end(&self);
    /// Change the scrollbar visibility mode at runtime.
    fn set_scrollbar(&self, scrollbar: Scrollbar);
    /// Mark the scrollbar active (hover/drag).
    fn set_scrollbar_active(&self, active: bool);
    /// Hide an expired transient (`auto`) scrollbar. Returns whether the
    /// visibility changed, so the caller can repaint.
    fn expire_transient_scrollbar(&self, now: Instant) -> bool;
    /// Render one scrollbar track cell.
    fn scrollbar_track_text(&self, text: &str) -> String;
    /// Render one scrollbar thumb cell.
    fn scrollbar_thumb_text(&self, text: &str) -> String;
}

struct ScrollViewState {
    scroll_top: i32,
    content_height: i32,
    viewport_height: i32,
    following_end: bool,
    follow_suppressed_at_end: bool,
    transient_scrollbar_visible: bool,
    scrollbar_active: bool,
    transient_deadline: Option<Instant>,
    request_render: Option<Rc<dyn Fn()>>,
}

impl std::fmt::Debug for ScrollViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollViewState")
            .field("scroll_top", &self.scroll_top)
            .field("content_height", &self.content_height)
            .field("viewport_height", &self.viewport_height)
            .field("following_end", &self.following_end)
            .field("follow_suppressed_at_end", &self.follow_suppressed_at_end)
            .field("transient_scrollbar_visible", &self.transient_scrollbar_visible)
            .field("scrollbar_active", &self.scrollbar_active)
            .field("transient_deadline", &self.transient_deadline)
            // The render callback is not printable; report its presence only.
            .field(
                "request_render",
                &self.request_render.as_ref().map(|_| "<callback>"),
            )
            .finish()
    }
}

impl Default for ScrollViewState {
    fn default() -> Self {
        Self {
            scroll_top: 0,
            content_height: 0,
            viewport_height: 0,
            following_end: false,
            follow_suppressed_at_end: false,
            transient_scrollbar_visible: false,
            scrollbar_active: false,
            transient_deadline: None,
            request_render: None,
        }
    }
}

impl ScrollViewState {
    fn max_scroll_top(&self) -> i32 {
        (self.content_height - self.viewport_height).max(0)
    }

    fn request_render(&self) {
        if let Some(callback) = &self.request_render {
            callback();
        }
    }

    fn mark_scrollbar_activity(&mut self, mode: Scrollbar, delay: Duration) {
        if mode != Scrollbar::Auto || self.content_height <= self.viewport_height {
            return;
        }
        self.transient_scrollbar_visible = true;
        self.transient_deadline = Some(Instant::now() + delay);
    }

    fn hide_transient_scrollbar(&mut self) {
        self.transient_scrollbar_visible = false;
        self.transient_deadline = None;
    }

    fn is_scrollbar_visible(&self, mode: Scrollbar) -> bool {
        match mode {
            Scrollbar::Always => self.viewport_height > 0,
            Scrollbar::Auto => {
                self.transient_scrollbar_visible && self.content_height > self.viewport_height
            }
            Scrollbar::Hidden => false,
        }
    }
}

/// A single-child viewport over a taller component.
pub struct ScrollView {
    child: Box<dyn Component>,
    state: RefCell<ScrollViewState>,
    follow_end: bool,
    primary: bool,
    overscroll: Overscroll,
    scrollbar: Cell<Scrollbar>,
    scrollbar_hide_delay: Duration,
    scrollbar_track_style: Option<Box<dyn Fn(&str) -> String>>,
    scrollbar_thumb_style: Option<Box<dyn Fn(&str) -> String>>,
}

impl ScrollView {
    /// Wrap `child` in a scroll view.
    pub fn new(child: Box<dyn Component>, options: ScrollViewOptions) -> Self {
        let ScrollViewOptions {
            follow_end,
            primary,
            overscroll,
            scrollbar,
            scrollbar_hide_delay,
            scrollbar_track_style,
            scrollbar_thumb_style,
        } = options;
        Self {
            child,
            state: RefCell::new(ScrollViewState {
                following_end: follow_end,
                ..ScrollViewState::default()
            }),
            follow_end,
            primary,
            overscroll,
            scrollbar: Cell::new(scrollbar),
            scrollbar_hide_delay,
            scrollbar_track_style,
            scrollbar_thumb_style,
        }
    }

    /// The scrolled content component.
    pub fn child(&self) -> &dyn Component {
        self.child.as_ref()
    }

    /// Change the transient scrollbar hide delay.
    pub fn set_scrollbar_hide_delay(&mut self, delay: Duration) {
        self.scrollbar_hide_delay = delay;
    }
}

impl Component for ScrollView {
    fn render(&self, width: u16) -> Vec<String> {
        let width = i32::from(width.max(1));
        let content_width = self.get_content_width(width);
        let lines = self.child.render(u16::try_from(content_width.max(1)).unwrap_or(u16::MAX));
        if content_width == width {
            lines
        } else {
            lines
                .into_iter()
                .map(|line| format!("{line} "))
                .collect()
        }
    }

    fn invalidate(&mut self) {
        self.child.invalidate();
    }

    fn layout_node(&self) -> Option<LayoutNode<'_>> {
        Some(LayoutNode::Scroll(ScrollLayoutNode {
            component: self.child.as_ref(),
            state: self,
        }))
    }
}

impl ScrollLayoutState for ScrollView {
    fn scroll_top(&self) -> i32 {
        self.state.borrow().scroll_top
    }

    fn is_following_end(&self) -> bool {
        self.state.borrow().following_end
    }

    fn content_height(&self) -> i32 {
        self.state.borrow().content_height
    }

    fn viewport_height(&self) -> i32 {
        self.state.borrow().viewport_height
    }

    fn is_primary(&self) -> bool {
        self.primary
    }

    fn overscroll(&self) -> Overscroll {
        self.overscroll
    }

    fn scrollbar(&self) -> Scrollbar {
        self.scrollbar.get()
    }

    fn is_scrollbar_visible(&self) -> bool {
        self.state.borrow().is_scrollbar_visible(self.scrollbar.get())
    }

    fn is_scrollbar_active(&self) -> bool {
        self.state.borrow().scrollbar_active
    }

    fn get_content_width(&self, width: i32) -> i32 {
        if self.scrollbar.get() == Scrollbar::Always && width > 1 {
            width - 1
        } else {
            width
        }
    }

    fn update_layout(
        &self,
        content_height: i32,
        viewport_height: i32,
        request_render: Rc<dyn Fn()>,
    ) {
        let mut state = self.state.borrow_mut();
        state.content_height = content_height.max(0);
        state.viewport_height = viewport_height.max(0);
        state.request_render = Some(request_render);
        let max_scroll_top = state.max_scroll_top();
        state.scroll_top = if state.following_end {
            max_scroll_top
        } else {
            state.scroll_top.max(0).min(max_scroll_top)
        };
        if state.scroll_top < max_scroll_top {
            state.follow_suppressed_at_end = false;
        }
        if self.follow_end && state.scroll_top == max_scroll_top && !state.follow_suppressed_at_end {
            state.following_end = true;
        }
        if state.content_height <= state.viewport_height {
            state.hide_transient_scrollbar();
        }
    }

    fn scroll_to(&self, scroll_top: i32, disable_follow: bool) {
        let mode = self.scrollbar.get();
        let delay = self.scrollbar_hide_delay;
        let mut state = self.state.borrow_mut();
        let max_scroll_top = state.max_scroll_top();
        let next = scroll_top.max(0).min(max_scroll_top);
        let next_follow_suppressed_at_end = disable_follow && next == max_scroll_top;
        let next_following_end = !next_follow_suppressed_at_end && self.follow_end && next == max_scroll_top;
        if next == state.scroll_top
            && next_following_end == state.following_end
            && next_follow_suppressed_at_end == state.follow_suppressed_at_end
        {
            return;
        }
        let moved = next != state.scroll_top;
        state.scroll_top = next;
        state.following_end = next_following_end;
        state.follow_suppressed_at_end = next_follow_suppressed_at_end;
        if moved {
            state.mark_scrollbar_activity(mode, delay);
        }
        drop(state);
        self.state.borrow().request_render();
    }

    fn scroll_by(&self, lines: i32) -> i32 {
        if lines == 0 {
            return 0;
        }
        let mode = self.scrollbar.get();
        let delay = self.scrollbar_hide_delay;
        let mut state = self.state.borrow_mut();
        let max_scroll_top = state.max_scroll_top();
        let start = if state.following_end {
            max_scroll_top
        } else {
            state.scroll_top
        };
        let next = (start + lines).max(0).min(max_scroll_top);
        let moved = next - start;
        let was_following_end = state.following_end;
        state.scroll_top = next;
        state.following_end = self.follow_end && next == max_scroll_top;
        state.follow_suppressed_at_end = false;
        if moved != 0 {
            state.mark_scrollbar_activity(mode, delay);
        }
        let repaint = moved != 0 || state.following_end != was_following_end;
        drop(state);
        if repaint {
            self.state.borrow().request_render();
        }
        lines - moved
    }

    fn scroll_to_start(&self) {
        let mode = self.scrollbar.get();
        let delay = self.scrollbar_hide_delay;
        let mut state = self.state.borrow_mut();
        let changed = state.scroll_top != 0
            || state.following_end != (self.follow_end && state.content_height <= state.viewport_height);
        state.scroll_top = 0;
        state.following_end = self.follow_end && state.content_height <= state.viewport_height;
        state.follow_suppressed_at_end = false;
        if changed {
            state.mark_scrollbar_activity(mode, delay);
        }
        drop(state);
        if changed {
            self.state.borrow().request_render();
        }
    }

    fn scroll_to_end(&self) {
        let mode = self.scrollbar.get();
        let delay = self.scrollbar_hide_delay;
        let mut state = self.state.borrow_mut();
        let next = state.max_scroll_top();
        let changed = state.scroll_top != next || state.following_end != self.follow_end;
        state.scroll_top = next;
        state.following_end = self.follow_end;
        state.follow_suppressed_at_end = false;
        if changed {
            state.mark_scrollbar_activity(mode, delay);
        }
        drop(state);
        if changed {
            self.state.borrow().request_render();
        }
    }

    fn set_scrollbar(&self, scrollbar: Scrollbar) {
        if scrollbar == self.scrollbar.get() {
            return;
        }
        self.scrollbar.set(scrollbar);
        let mark_active = {
            let mut state = self.state.borrow_mut();
            if scrollbar != Scrollbar::Auto {
                state.hide_transient_scrollbar();
                false
            } else {
                state.scrollbar_active
            }
        };
        if mark_active {
            let delay = self.scrollbar_hide_delay;
            self.state
                .borrow_mut()
                .mark_scrollbar_activity(Scrollbar::Auto, delay);
        }
        self.state.borrow().request_render();
    }

    fn set_scrollbar_active(&self, active: bool) {
        {
            let mut state = self.state.borrow_mut();
            if active == state.scrollbar_active {
                return;
            }
            state.scrollbar_active = active;
            let delay = self.scrollbar_hide_delay;
            state.mark_scrollbar_activity(Scrollbar::Auto, delay);
        }
        self.state.borrow().request_render();
    }

    fn expire_transient_scrollbar(&self, now: Instant) -> bool {
        let mut state = self.state.borrow_mut();
        let expired = state
            .transient_deadline
            .is_some_and(|deadline| deadline <= now);
        if !expired {
            return false;
        }
        state.transient_deadline = None;
        if self.scrollbar.get() != Scrollbar::Auto || state.scrollbar_active {
            return false;
        }
        state.transient_scrollbar_visible = false;
        true
    }

    fn scrollbar_track_text(&self, text: &str) -> String {
        match &self.scrollbar_track_style {
            Some(style) => style(text),
            None => format!("\x1b[90m{text}\x1b[39m"),
        }
    }

    fn scrollbar_thumb_text(&self, text: &str) -> String {
        match &self.scrollbar_thumb_style {
            Some(style) => style(text),
            None => format!("\x1b[37m{text}\x1b[39m"),
        }
    }
}

struct OwnedEntry {
    component: Box<dyn Component>,
    basis: StackBasis,
    grow: i32,
    shrink: i32,
    min_size: i32,
    max_size: i32,
    visible: Option<Box<dyn Fn(LayoutViewport) -> bool>>,
}

impl OwnedEntry {
    fn from_options(component: Box<dyn Component>, options: StackEntryOptions) -> Self {
        Self {
            component,
            basis: options.basis.unwrap_or_default(),
            grow: options.grow.unwrap_or(0),
            shrink: options.shrink.unwrap_or(1),
            min_size: options.min_size.unwrap_or(0),
            max_size: options.max_size.unwrap_or(i32::MAX),
            visible: options.visible,
        }
    }

    fn resolved(&self) -> StackLayoutEntry<'_> {
        StackLayoutEntry {
            component: self.component.as_ref(),
            basis: self.basis,
            grow: self.grow,
            shrink: self.shrink,
            min_size: self.min_size,
            max_size: self.max_size,
            visible: self.visible.as_deref(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StackKind {
    Vertical,
    Horizontal,
}

struct Stack {
    kind: StackKind,
    entries: Vec<OwnedEntry>,
    gap: i32,
    align: StackAlign,
}

impl Stack {
    fn new(kind: StackKind) -> Self {
        Self {
            kind,
            entries: Vec::new(),
            gap: 0,
            align: StackAlign::Stretch,
        }
    }

    fn push(&mut self, component: Box<dyn Component>, options: StackEntryOptions) {
        self.entries.push(OwnedEntry::from_options(component, options));
    }

    fn resolved_entries(&self) -> Vec<StackLayoutEntry<'_>> {
        self.entries
            .iter()
            .map(OwnedEntry::resolved)
            .collect::<Vec<_>>()
    }

    fn layout_node(&self) -> LayoutNode<'_> {
        let node = StackLayoutNode {
            entries: self.resolved_entries(),
            gap: self.gap,
            align: self.align,
        };
        match self.kind {
            StackKind::Vertical => LayoutNode::VStack(node),
            StackKind::Horizontal => LayoutNode::HStack(node),
        }
    }
}

/// Vertical stack: children are laid out top to bottom.
pub struct VStack {
    stack: Stack,
}

impl Default for VStack {
    fn default() -> Self {
        Self::new()
    }
}

impl VStack {
    /// An empty vertical stack.
    pub fn new() -> Self {
        Self {
            stack: Stack::new(StackKind::Vertical),
        }
    }

    /// Set the gap between adjacent children.
    pub fn with_gap(mut self, gap: i32) -> Self {
        self.stack.gap = gap.max(0);
        self
    }

    /// Set cross-axis alignment.
    pub fn with_align(mut self, align: StackAlign) -> Self {
        self.stack.align = align;
        self
    }

    /// Append a child with default sizing.
    pub fn push(&mut self, component: Box<dyn Component>) {
        self.stack.push(component, StackEntryOptions::new());
    }

    /// Append a child with explicit sizing.
    pub fn push_with(&mut self, component: Box<dyn Component>, options: StackEntryOptions) {
        self.stack.push(component, options);
    }

    /// Number of children.
    pub fn len(&self) -> usize {
        self.stack.entries.len()
    }

    /// Whether the stack has no children.
    pub fn is_empty(&self) -> bool {
        self.stack.entries.is_empty()
    }
}

impl Component for VStack {
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in &self.stack.entries {
            lines.extend(entry.component.render(width));
        }
        lines
    }

    fn invalidate(&mut self) {
        for entry in &mut self.stack.entries {
            entry.component.invalidate();
        }
    }

    fn layout_node(&self) -> Option<LayoutNode<'_>> {
        Some(self.stack.layout_node())
    }
}

/// Horizontal stack: children are laid out left to right.
pub struct HStack {
    stack: Stack,
}

impl Default for HStack {
    fn default() -> Self {
        Self::new()
    }
}

impl HStack {
    /// An empty horizontal stack.
    pub fn new() -> Self {
        Self {
            stack: Stack::new(StackKind::Horizontal),
        }
    }

    /// Set the gap between adjacent children.
    pub fn with_gap(mut self, gap: i32) -> Self {
        self.stack.gap = gap.max(0);
        self
    }

    /// Set cross-axis alignment.
    pub fn with_align(mut self, align: StackAlign) -> Self {
        self.stack.align = align;
        self
    }

    /// Append a child with default sizing.
    pub fn push(&mut self, component: Box<dyn Component>) {
        self.stack.push(component, StackEntryOptions::new());
    }

    /// Append a child with explicit sizing.
    pub fn push_with(&mut self, component: Box<dyn Component>, options: StackEntryOptions) {
        self.stack.push(component, options);
    }

    /// Number of children.
    pub fn len(&self) -> usize {
        self.stack.entries.len()
    }

    /// Whether the stack has no children.
    pub fn is_empty(&self) -> bool {
        self.stack.entries.is_empty()
    }
}

impl Component for HStack {
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        for entry in &self.stack.entries {
            lines.extend(entry.component.render(width));
        }
        lines
    }

    fn invalidate(&mut self) {
        for entry in &mut self.stack.entries {
            entry.component.invalidate();
        }
    }

    fn layout_node(&self) -> Option<LayoutNode<'_>> {
        Some(self.stack.layout_node())
    }
}

/// One resolved box in the last rendered frame.
pub struct LayoutBox<'a> {
    /// The component this box was rendered from.
    pub component: &'a dyn Component,
    /// Allocated rectangle in frame coordinates.
    pub rect: LayoutRect,
    /// Visible rectangle after intersecting every ancestor clip.
    pub clip: LayoutRect,
    /// Child boxes in paint order.
    pub children: Vec<LayoutBox<'a>>,
    /// Rendered lines for a leaf; `None` for layout containers.
    pub lines: Option<Rc<Vec<String>>>,
    /// First visible source line of `lines` (cursor-aware clipping).
    pub line_offset: usize,
    /// Scroll state when the box is a scroll viewport.
    pub scroll_view: Option<&'a dyn ScrollLayoutState>,
    /// Complete scrolled content for the viewport's child.
    pub scroll_content_lines: Option<Rc<Vec<String>>>,
}

/// The resolved frame: painted rows plus the boxes that produced them.
pub struct LayoutFrame<'a> {
    /// Root box.
    pub root: LayoutBox<'a>,
    /// Frame width in cells.
    pub width: i32,
    /// Frame height in rows.
    pub height: i32,
    /// Painted rows, exactly `height` entries.
    pub lines: Vec<String>,
    /// The primary scroll view, when the frame contains one.
    pub primary_scroll_view: Option<&'a dyn ScrollLayoutState>,
}

impl<'a> LayoutFrame<'a> {
    /// Visual hit path from the deepest box to the root.
    pub fn boxes_at(&self, x: i32, y: i32) -> Vec<&LayoutBox<'a>> {
        fn visit<'b, 'a>(
            box_: &'b LayoutBox<'a>,
            depth: usize,
            x: i32,
            y: i32,
            out: &mut Vec<(&'b LayoutBox<'a>, usize)>,
        ) {
            if !box_.clip.contains(x, y) {
                return;
            }
            out.push((box_, depth));
            for child in &box_.children {
                visit(child, depth + 1, x, y, out);
            }
        }
        let mut found: Vec<(&LayoutBox<'a>, usize)> = Vec::new();
        visit(&self.root, 0, x, y, &mut found);
        found.sort_by(|left, right| right.1.cmp(&left.1));
        found.into_iter().map(|(box_, _)| box_).collect()
    }

    /// Scroll views whose box contains the point, deepest first.
    pub fn scroll_views_at(&self, x: i32, y: i32) -> Vec<&'a dyn ScrollLayoutState> {
        let mut found: Vec<(&LayoutBox<'a>, usize)> = Vec::new();
        fn visit<'b, 'a>(
            box_: &'b LayoutBox<'a>,
            depth: usize,
            x: i32,
            y: i32,
            out: &mut Vec<(&'b LayoutBox<'a>, usize)>,
        ) {
            if !box_.clip.contains(x, y) {
                return;
            }
            if box_.scroll_view.is_some() && box_.rect.contains(x, y) {
                out.push((box_, depth));
            }
            for child in &box_.children {
                visit(child, depth + 1, x, y, out);
            }
        }
        visit(&self.root, 0, x, y, &mut found);
        found.sort_by(|left, right| right.1.cmp(&left.1));
        found
            .into_iter()
            .filter_map(|(box_, _)| box_.scroll_view)
            .collect()
    }

    /// The box that owns a scroll view.
    pub fn scroll_view_box(&self, scroll_view: &dyn ScrollLayoutState) -> Option<&LayoutBox<'a>> {
        fn visit<'b, 'a>(
            box_: &'b LayoutBox<'a>,
            scroll_view: &dyn ScrollLayoutState,
        ) -> Option<&'b LayoutBox<'a>> {
            if box_
                .scroll_view
                .is_some_and(|candidate| std::ptr::eq(candidate, scroll_view))
            {
                return Some(box_);
            }
            for child in &box_.children {
                if let Some(found) = visit(child, scroll_view) {
                    return Some(found);
                }
            }
            None
        }
        visit(&self.root, scroll_view)
    }

    /// Lowest box in the hit path.
    pub fn deepest_component_identity(&self, x: i32, y: i32) -> Option<usize> {
        self.boxes_at(x, y)
            .first()
            .map(|box_| component_identity(box_.component))
    }

    /// Resolve a live component from its retained identity.
    pub fn component_by_identity(&self, identity: usize) -> Option<&'a dyn Component> {
        fn visit<'a>(box_: &LayoutBox<'a>, identity: usize) -> Option<&'a dyn Component> {
            if component_identity(box_.component) == identity {
                return Some(box_.component);
            }
            for child in &box_.children {
                if let Some(found) = visit(child, identity) {
                    return Some(found);
                }
            }
            None
        }
        visit(&self.root, identity)
    }

    /// Dispatch an event to the deepest component that handles it.
    pub fn dispatch_at(&self, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult<'a>> {
        for box_ in self.boxes_at(event.screen_x, event.screen_y) {
            let target = crate::mouse::TuiMouseDispatchTarget {
                component: box_.component,
                origin_x: box_.rect.x,
                origin_y: box_.rect.y,
                width: box_.rect.width,
                height: box_.rect.height,
            };
            if let Some(result) = dispatch_mouse_event(box_.component, &event.retarget(&target)) {
                return Some(result);
            }
        }
        None
    }
}

/// Scrollbar geometry resolved from a box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollbarGeometry {
    /// Column the scrollbar occupies.
    pub column: i32,
    /// First row of the track.
    pub track_top: i32,
    /// Track height in rows.
    pub track_height: i32,
    /// First row of the thumb.
    pub thumb_top: i32,
    /// Thumb height in rows.
    pub thumb_height: i32,
    /// Largest scroll offset.
    pub max_scroll_top: i32,
}

/// Resolve scrollbar geometry for a scroll viewport box.
pub fn scrollbar_geometry(box_: &LayoutBox<'_>, include_hidden_auto: bool) -> Option<ScrollbarGeometry> {
    let scroll_view = box_.scroll_view?;
    if box_.rect.width <= 0 || box_.rect.height <= 0 {
        return None;
    }
    let content_height = box_
        .children
        .first()
        .map(|child| child.rect.height)
        .or_else(|| {
            box_.scroll_content_lines
                .as_ref()
                .map(|lines| i32::try_from(lines.len()).unwrap_or(i32::MAX))
        })
        .unwrap_or(0);
    let track_height = box_.rect.height;
    let can_reveal_hidden_auto =
        include_hidden_auto && scroll_view.scrollbar() == Scrollbar::Auto && content_height > track_height;
    if !scroll_view.is_scrollbar_visible() && !can_reveal_hidden_auto {
        return None;
    }
    let min_thumb_height = track_height.min(2);
    let thumb_height = if content_height <= 0 {
        track_height
    } else {
        let proportional =
            ((i64::from(track_height) * i64::from(track_height)) as f64 / f64::from(content_height)).round()
                as i64;
        i32::try_from(proportional)
            .unwrap_or(track_height)
            .clamp(min_thumb_height, track_height)
    };
    let max_scroll_top = (content_height - track_height).max(0);
    let max_thumb_top = track_height - thumb_height;
    let thumb_offset = if max_scroll_top == 0 {
        0
    } else {
        ((f64::from(scroll_view.scroll_top()) / f64::from(max_scroll_top)) * f64::from(max_thumb_top)).round()
            as i32
    };
    let column = box_.rect.x + box_.rect.width - 1;
    if column < box_.clip.x || column >= box_.clip.x + box_.clip.width {
        return None;
    }
    Some(ScrollbarGeometry {
        column,
        track_top: box_.rect.y,
        track_height,
        thumb_top: box_.rect.y + thumb_offset,
        thumb_height,
        max_scroll_top,
    })
}

fn intersect(first: LayoutRect, second: LayoutRect) -> LayoutRect {
    let x = first.x.max(second.x);
    let y = first.y.max(second.y);
    let right = (first.x + first.width).min(second.x + second.width);
    let bottom = (first.y + first.height).min(second.y + second.height);
    LayoutRect {
        x,
        y,
        width: (right - x).max(0),
        height: (bottom - y).max(0),
    }
}

/// Pi's `allocateStackSizes`: basis, then grow/shrink distribution.
pub fn allocate_stack_sizes(
    entries: &[StackLayoutEntry<'_>],
    intrinsic_sizes: &[i32],
    available_size: Option<i32>,
    gap: i32,
) -> Vec<i32> {
    let mut sizes = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let requested = match entry.basis {
                StackBasis::Fixed(value) => value,
                StackBasis::Auto => intrinsic_sizes.get(index).copied().unwrap_or(0),
            };
            clamp_size(requested, entry)
        })
        .collect::<Vec<_>>();
    let Some(available) = available_size else {
        return sizes;
    };
    let gaps = i32::try_from(entries.len().saturating_sub(1))
        .unwrap_or(i32::MAX)
        .saturating_mul(gap.max(0));
    let content_size = (available.max(0) - gaps).max(0);
    let total: i32 = sizes.iter().copied().fold(0, i32::saturating_add);
    if total < content_size {
        distribute(&mut sizes, entries, content_size - total, Growth::Grow);
    } else if total > content_size {
        distribute(&mut sizes, entries, total - content_size, Growth::Shrink);
    }
    sizes
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Growth {
    Grow,
    Shrink,
}

fn clamp_size(size: i32, entry: &StackLayoutEntry<'_>) -> i32 {
    let min = entry.min_size.max(0);
    let max = entry.max_size.max(min);
    size.max(0).min(max).max(min)
}

fn distribute(sizes: &mut [i32], entries: &[StackLayoutEntry<'_>], amount: i32, mode: Growth) {
    let mut remaining = amount;
    while remaining > 0 {
        let mut total_weight: i64 = 0;
        for (index, entry) in entries.iter().enumerate() {
            let size = sizes[index];
            let weight = match mode {
                Growth::Grow => {
                    if entry.grow > 0 && size < entry.max_size {
                        i64::from(entry.grow)
                    } else {
                        0
                    }
                }
                Growth::Shrink => {
                    if entry.shrink > 0 && size > entry.min_size {
                        i64::from(entry.shrink) * i64::from(size.max(1))
                    } else {
                        0
                    }
                }
            };
            total_weight += weight;
        }
        if total_weight == 0 {
            return;
        }
        let mut distributed = 0;
        for (index, entry) in entries.iter().enumerate() {
            if remaining <= 0 {
                break;
            }
            let size = sizes[index];
            let weight = match mode {
                Growth::Grow => {
                    if entry.grow > 0 && size < entry.max_size {
                        i64::from(entry.grow)
                    } else {
                        continue;
                    }
                }
                Growth::Shrink => {
                    if entry.shrink > 0 && size > entry.min_size {
                        i64::from(entry.shrink) * i64::from(size.max(1))
                    } else {
                        continue;
                    }
                }
            };
            let proposed = ((i64::from(remaining) * weight) / total_weight).max(1);
            let proposed = i32::try_from(proposed).unwrap_or(remaining);
            let capacity = match mode {
                Growth::Grow => entry.max_size.saturating_sub(size),
                Growth::Shrink => size.saturating_sub(entry.min_size),
            };
            let delta = remaining.min(proposed).min(capacity);
            if delta <= 0 {
                continue;
            }
            sizes[index] = match mode {
                Growth::Grow => size.saturating_add(delta),
                Growth::Shrink => size.saturating_sub(delta),
            };
            remaining -= delta;
            distributed += delta;
        }
        if distributed == 0 {
            return;
        }
    }
}

struct LayoutContext<'a> {
    viewport: LayoutViewport,
    render_cache: RefCell<HashMap<(usize, i32), Rc<Vec<String>>>>,
    request_render: Rc<dyn Fn()>,
    primary_scroll_view: RefCell<Option<&'a dyn ScrollLayoutState>>,
}

impl<'a> LayoutContext<'a> {
    fn render_cached(&self, component: &'a dyn Component, width: i32) -> Rc<Vec<String>> {
        let width = width.max(1);
        let key = (component_identity(component), width);
        if let Some(lines) = self.render_cache.borrow().get(&key) {
            return lines.clone();
        }
        let lines = Rc::new(component.render(u16::try_from(width).unwrap_or(u16::MAX)));
        self.render_cache.borrow_mut().insert(key, lines.clone());
        lines
    }

    fn measure_height(&self, component: &'a dyn Component, width: i32) -> i32 {
        i32::try_from(self.render_cached(component, width).len()).unwrap_or(i32::MAX)
    }

    fn measure_width(&self, component: &'a dyn Component, width: i32) -> i32 {
        self.render_cached(component, width)
            .iter()
            .map(|line| i32::try_from(visible_width(line)).unwrap_or(i32::MAX))
            .max()
            .unwrap_or(0)
    }

    fn note_primary(&self, scroll_view: &'a dyn ScrollLayoutState) {
        let mut primary = self.primary_scroll_view.borrow_mut();
        if primary.is_none() || scroll_view.is_primary() {
            *primary = Some(scroll_view);
        }
    }
}

fn translate_box(box_: &mut LayoutBox<'_>, delta_y: i32) {
    box_.rect.y += delta_y;
    for child in &mut box_.children {
        translate_box(child, delta_y);
    }
}

fn update_clips(box_: &mut LayoutBox<'_>, parent_clip: LayoutRect) {
    box_.clip = intersect(parent_clip, box_.rect);
    let clip = box_.clip;
    for child in &mut box_.children {
        update_clips(child, clip);
    }
}

fn layout_component<'a>(
    context: &LayoutContext<'a>,
    component: &'a dyn Component,
    x: i32,
    y: i32,
    width: i32,
    height: Option<i32>,
    clip: LayoutRect,
) -> LayoutBox<'a> {
    let safe_width = width.max(1);
    let Some(node) = component.layout_node() else {
        let lines = context.render_cached(component, safe_width);
        let allocated_height = height.unwrap_or_else(|| i32::try_from(lines.len()).unwrap_or(i32::MAX));
        let mut line_offset = 0;
        if lines.len() > usize::try_from(allocated_height).unwrap_or(0) && allocated_height > 0 {
            if let Some(cursor_line) = lines.iter().position(|line| line.contains(crate::tui::CURSOR_MARKER)) {
                let cursor_line = i32::try_from(cursor_line).unwrap_or(i32::MAX);
                if cursor_line >= allocated_height {
                    line_offset = usize::try_from(cursor_line - allocated_height + 1).unwrap_or(0);
                }
            }
        }
        let rect = LayoutRect {
            x,
            y,
            width: safe_width,
            height: allocated_height,
        };
        return LayoutBox {
            component,
            rect,
            clip: intersect(clip, rect),
            children: Vec::new(),
            lines: Some(lines),
            line_offset,
            scroll_view: None,
            scroll_content_lines: None,
        };
    };

    match node {
        LayoutNode::Scroll(node) => {
            let previous_scroll_top = node.state.scroll_top();
            let content_width = node.state.get_content_width(safe_width);
            let mut child_box = layout_component(
                context,
                node.component,
                x,
                y - previous_scroll_top,
                content_width,
                None,
                clip,
            );
            let content_height = child_box.rect.height;
            let viewport_height = height.unwrap_or(content_height).max(0);
            node.state
                .update_layout(content_height, viewport_height, context.request_render.clone());
            translate_box(&mut child_box, previous_scroll_top - node.state.scroll_top());
            context.note_primary(node.state);
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: viewport_height,
            };
            let child_clip = intersect(clip, rect);
            let mut box_ = LayoutBox {
                component,
                rect,
                clip: child_clip,
                children: vec![child_box],
                lines: None,
                line_offset: 0,
                scroll_view: Some(node.state),
                scroll_content_lines: Some(context.render_cached(node.component, content_width)),
            };
            let child_clip = box_.clip;
            if let Some(child) = box_.children.first_mut() {
                update_clips(child, child_clip);
            }
            box_
        }
        LayoutNode::VStack(node) => {
            let entries = node
                .entries
                .iter()
                .filter(|entry| entry.is_visible(context.viewport))
                .collect::<Vec<_>>();
            let gap = node.gap.max(0);
            let gap_total = i32::try_from(entries.len().saturating_sub(1))
                .unwrap_or(i32::MAX)
                .saturating_mul(gap);
            let intrinsic = entries
                .iter()
                .map(|entry| match entry.basis {
                    StackBasis::Fixed(value) => value,
                    StackBasis::Auto => context.measure_height(entry.component, safe_width),
                })
                .collect::<Vec<_>>();
            // A resolved entry is a small `Copy` record (a component reference
            // and scalar sizing fields), so assembling the owned slice the
            // sizing pass expects copies it; nothing large is duplicated.
            let resolved = entries.iter().map(|entry| **entry).collect::<Vec<_>>();
            let sizes = allocate_stack_sizes(&resolved, &intrinsic, height, gap);
            let natural: i32 = sizes.iter().copied().fold(0, i32::saturating_add).saturating_add(gap_total);
            let allocated_height = height.unwrap_or(natural).max(0);
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: allocated_height,
            };
            let mut box_ = LayoutBox {
                component,
                rect,
                clip: intersect(clip, rect),
                children: Vec::new(),
                lines: None,
                line_offset: 0,
                scroll_view: None,
                scroll_content_lines: None,
            };
            let mut child_y = y;
            for (index, entry) in entries.iter().enumerate() {
                let child = layout_component(
                    context,
                    entry.component,
                    x,
                    child_y,
                    safe_width,
                    sizes.get(index).copied(),
                    box_.clip,
                );
                child_y = child_y
                    .saturating_add(sizes.get(index).copied().unwrap_or(0))
                    .saturating_add(gap);
                box_.children.push(child);
            }
            box_
        }
        LayoutNode::HStack(node) => {
            let entries = node
                .entries
                .iter()
                .filter(|entry| entry.is_visible(context.viewport))
                .collect::<Vec<_>>();
            let gap = node.gap.max(0);
            let intrinsic_widths = entries
                .iter()
                .map(|entry| match entry.basis {
                    StackBasis::Fixed(value) => value,
                    StackBasis::Auto => context.measure_width(entry.component, safe_width),
                })
                .collect::<Vec<_>>();
            // See the vertical-stack arm: entries are cheap `Copy` records.
            let resolved = entries.iter().map(|entry| **entry).collect::<Vec<_>>();
            let widths = allocate_stack_sizes(&resolved, &intrinsic_widths, Some(safe_width), gap);
            let intrinsic_heights = entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    context.measure_height(entry.component, widths.get(index).copied().unwrap_or(1).max(1))
                })
                .collect::<Vec<_>>();
            let allocated_height =
                height.unwrap_or_else(|| intrinsic_heights.iter().copied().max().unwrap_or(0));
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: allocated_height,
            };
            let mut box_ = LayoutBox {
                component,
                rect,
                clip: intersect(clip, rect),
                children: Vec::new(),
                lines: None,
                line_offset: 0,
                scroll_view: None,
                scroll_content_lines: None,
            };
            let mut child_x = x;
            for (index, entry) in entries.iter().enumerate() {
                let natural_child_height = intrinsic_heights.get(index).copied().unwrap_or(0);
                let child_height = if node.align == StackAlign::Stretch {
                    allocated_height
                } else {
                    allocated_height.min(natural_child_height)
                };
                let mut child_y = y;
                match node.align {
                    StackAlign::Center => {
                        child_y += (allocated_height - child_height) / 2;
                    }
                    StackAlign::End => {
                        child_y += allocated_height - child_height;
                    }
                    StackAlign::Stretch | StackAlign::Start => {}
                }
                let child_width = widths.get(index).copied().unwrap_or(0);
                if child_width == 0 {
                    box_.children.push(LayoutBox {
                        component: entry.component,
                        rect: LayoutRect {
                            x: child_x,
                            y: child_y,
                            width: 0,
                            height: child_height,
                        },
                        clip: LayoutRect {
                            x: child_x,
                            y: child_y,
                            width: 0,
                            height: 0,
                        },
                        children: Vec::new(),
                        lines: None,
                        line_offset: 0,
                        scroll_view: None,
                        scroll_content_lines: None,
                    });
                } else {
                    let child = layout_component(
                        context,
                        entry.component,
                        child_x,
                        child_y,
                        child_width,
                        Some(child_height),
                        box_.clip,
                    );
                    box_.children.push(child);
                }
                child_x = child_x.saturating_add(child_width).saturating_add(gap);
            }
            box_
        }
    }
}

fn composite_row(row: &str, source: &str, x: i32, width: i32, total_width: i32) -> String {
    if x <= 0 && width >= total_width {
        return source.to_owned();
    }
    let x = x.max(0);
    let width = width.min(total_width - x).max(0);
    if width == 0 {
        return row.to_owned();
    }
    let x_cells = usize::try_from(x).unwrap_or(0);
    let width_cells = usize::try_from(width).unwrap_or(0);
    let mut out = String::new();
    let before = slice_by_column(row, 0, x_cells, true);
    let before_width = visible_width(&before);
    out.push_str(&before);
    if before_width < x_cells {
        out.push_str(&" ".repeat(x_cells - before_width));
    }
    let middle = slice_by_column(source, 0, width_cells, true);
    let middle_width = visible_width(&middle);
    out.push_str(&middle);
    if middle_width < width_cells {
        out.push_str(&" ".repeat(width_cells - middle_width));
    }
    let after_start = x_cells + width_cells;
    let after_length = usize::try_from((total_width - x - width).max(0)).unwrap_or(0);
    out.push_str(&slice_by_column(row, after_start, after_length, true));
    out
}

fn replace_scrollbar_cell(line: &str, column: i32, total_width: i32, replacement: &str) -> String {
    if column < 0 || column >= total_width {
        return line.to_owned();
    }
    let column = usize::try_from(column).unwrap_or(0);
    let total_width = usize::try_from(total_width).unwrap_or(0);
    let before = slice_by_column(line, 0, column, true);
    let before_width = visible_width(&before);
    let after = slice_by_column(line, column + 1, total_width.saturating_sub(column + 1), true);
    let mut out = String::new();
    out.push_str(&before);
    if before_width < column {
        out.push_str(&" ".repeat(column - before_width));
    }
    out.push_str("\x1b[0m");
    out.push_str(replacement);
    out.push_str(&after);
    out
}

fn paint_scrollbar(box_: &LayoutBox<'_>, screen: &mut [String], total_width: i32) {
    let Some(scroll_view) = box_.scroll_view else {
        return;
    };
    let Some(geometry) = scrollbar_geometry(box_, false) else {
        return;
    };
    for offset in 0..geometry.track_height {
        let row = geometry.track_top + offset;
        if row < box_.clip.y
            || row >= box_.clip.y + box_.clip.height
            || row < 0
            || row >= i32::try_from(screen.len()).unwrap_or(i32::MAX)
        {
            continue;
        }
        let is_thumb =
            row >= geometry.thumb_top && row < geometry.thumb_top + geometry.thumb_height;
        let replacement = if is_thumb {
            let glyph = if scroll_view.is_scrollbar_active() {
                "█"
            } else {
                "┃"
            };
            scroll_view.scrollbar_thumb_text(glyph)
        } else {
            scroll_view.scrollbar_track_text("│")
        };
        let index = usize::try_from(row).unwrap_or(0);
        let current = screen.get(index).cloned().unwrap_or_default();
        screen[index] = replace_scrollbar_cell(
            &current,
            geometry.column,
            total_width,
            &replacement,
        );
    }
}

fn paint_box(box_: &LayoutBox<'_>, screen: &mut [String], total_width: i32) {
    if let Some(lines) = &box_.lines {
        let first_row = box_.rect.y.max(box_.clip.y).max(0);
        let last_row = (box_.rect.y + box_.rect.height)
            .min(box_.clip.y + box_.clip.height)
            .min(i32::try_from(screen.len()).unwrap_or(i32::MAX));
        let mut row = first_row;
        while row < last_row {
            let source_index = i64::try_from(box_.line_offset).unwrap_or(0)
                + i64::from(row - box_.rect.y);
            if source_index >= 0 {
                if let Some(source) = lines.get(usize::try_from(source_index).unwrap_or(usize::MAX)) {
                    let index = usize::try_from(row).unwrap_or(0);
                    let current = screen.get(index).cloned().unwrap_or_default();
                    let full_width = box_.rect.x <= 0 && box_.rect.width >= total_width;
                    screen[index] = if full_width && current.is_empty() {
                        source.clone()
                    } else {
                        composite_row(&current, source, box_.rect.x, box_.rect.width, total_width)
                    };
                }
            }
            row += 1;
        }
    }
    for child in &box_.children {
        paint_box(child, screen, total_width);
    }
    paint_scrollbar(box_, screen, total_width);
}

/// Resolve and paint the frame for a layout root.
pub fn render_layout_frame<'a>(
    root: &'a dyn Component,
    width: u16,
    height: u16,
    request_render: Rc<dyn Fn()>,
) -> LayoutFrame<'a> {
    let safe_width = i32::from(width.max(1));
    let safe_height = i32::from(height.max(1));
    let context = LayoutContext {
        viewport: LayoutViewport {
            width: safe_width,
            height: safe_height,
        },
        render_cache: RefCell::new(HashMap::new()),
        request_render,
        primary_scroll_view: RefCell::new(None),
    };
    let root_box = layout_component(
        &context,
        root,
        0,
        0,
        safe_width,
        Some(safe_height),
        LayoutRect {
            x: 0,
            y: 0,
            width: safe_width,
            height: safe_height,
        },
    );
    let mut lines = vec![String::new(); usize::try_from(safe_height).unwrap_or(0)];
    paint_box(&root_box, &mut lines, safe_width);
    // Copy the retained primary view out of its `Ref` before the frame literal:
    // the borrow must end while the frame is still a local, not at the tail
    // expression of this block.
    let primary_scroll_view = *context.primary_scroll_view.borrow();
    let frame = LayoutFrame {
        root: root_box,
        width: safe_width,
        height: safe_height,
        lines,
        primary_scroll_view,
    };
    frame
}

/// Route a wheel delta through the nested scroll views under the pointer and
/// return the delta that nobody consumed.
///
/// Deepest scroll view first; each view either consumes the delta or returns
/// the remainder. `Chain` passes the remainder outward and, when no visited
/// view consumed it, the layout's primary scroll view absorbs it. `Contain`
/// ends routing at that boundary: neither the enclosing views nor the primary
/// view may take the remainder, so it is returned to the caller (the product
/// sees it as `MouseRouting::wheel_remaining`).
///
/// The containment rule is deliberately stricter than Pi's raw `routeWheel`,
/// which applies the primary fallback after a `contain` break. In Pi's own
/// layouts the primary usually encloses the pointer, so that fallback made
/// `contain` a no-op for exactly the nested-block case the flag exists for; this
/// port honours the explicit boundary and lets the caller decide.
pub fn route_wheel_delta(frame: &LayoutFrame<'_>, x: i32, y: i32, delta: i32) -> i32 {
    let mut remaining = delta;
    let mut seen: Vec<usize> = Vec::new();
    let mut contained = false;
    for scroll_view in frame.scroll_views_at(x, y) {
        seen.push(std::ptr::from_ref(scroll_view) as *const () as usize);
        remaining = scroll_view.scroll_by(remaining);
        if remaining == 0 {
            break;
        }
        if scroll_view.overscroll() == Overscroll::Contain {
            contained = true;
            break;
        }
    }
    if remaining != 0 && !contained {
        if let Some(primary) = frame.primary_scroll_view {
            let identity = std::ptr::from_ref(primary) as *const () as usize;
            if !seen.contains(&identity) {
                remaining = primary.scroll_by(remaining);
            }
        }
    }
    remaining
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::Component;

    struct Lines {
        lines: Vec<String>,
    }

    impl Lines {
        fn new(lines: &[&str]) -> Self {
            Self {
                lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            }
        }

        fn rows(count: usize) -> Self {
            Self {
                lines: (0..count).map(|index| format!("row {index}")).collect(),
            }
        }
    }

    impl Component for Lines {
        fn render(&self, width: u16) -> Vec<String> {
            let width = usize::from(width);
            // A leaf reports its own content width; padding to the viewport
            // would make `measure_width` read the viewport instead of the
            // content and break every intrinsic-width expectation.
            self.lines
                .iter()
                .map(|line| {
                    if visible_width(line) <= width {
                        line.clone()
                    } else {
                        slice_by_column(line, 0, width, true)
                    }
                })
                .collect()
        }

        fn invalidate(&mut self) {}
    }

    fn frame_of<'a>(root: &'a dyn Component, width: u16, height: u16) -> LayoutFrame<'a> {
        render_layout_frame(root, width, height, Rc::new(|| {}))
    }

    #[test]
    fn vstack_children_take_their_intrinsic_height_without_spare_space() {
        let mut stack = VStack::new();
        stack.push(Box::new(Lines::new(&["a", "b"])));
        stack.push(Box::new(Lines::new(&["c"])));
        let frame = frame_of(&stack, 20, 10);
        assert_eq!(frame.root.children.len(), 2);
        assert_eq!(frame.root.children[0].rect.height, 2);
        assert_eq!(frame.root.children[1].rect.height, 1);
        assert_eq!(frame.root.children[1].rect.y, 2);
        assert_eq!(frame.root.rect.height, 10);
    }

    #[test]
    fn grow_shrink_and_limits_are_clamped_against_the_constrained_height() {
        let mut stack = VStack::new();
        stack.push_with(
            Box::new(Lines::new(&["fixed"])),
            StackEntryOptions::new()
                .with_basis(StackBasis::Fixed(2))
                .with_grow(1)
                .with_max_size(3),
        );
        // A fixed basis overrides the child's own 30-row render, and the pair
        // (2 + 1) is deliberately under-subscribed so the sizing pass grows
        // instead of shrinking.
        stack.push_with(
            Box::new(Lines::rows(30)),
            StackEntryOptions::new()
                .with_basis(StackBasis::Fixed(1))
                .with_grow(3),
        );
        let frame = frame_of(&stack, 20, 10);
        assert_eq!(frame.root.children[0].rect.height, 3, "max size caps growth");
        assert_eq!(frame.root.children[1].rect.height, 7, "grow fills the remainder");
        assert_eq!(frame.root.children[1].rect.y, 3);

        let mut shrink = VStack::new();
        shrink.push(Box::new(Lines::rows(6)));
        shrink.push_with(
            Box::new(Lines::rows(6)),
            StackEntryOptions::new().with_shrink(1).with_min_size(4),
        );
        let frame = frame_of(&shrink, 20, 4);
        assert_eq!(frame.root.children[0].rect.height, 0);
        assert_eq!(frame.root.children[1].rect.height, 4, "min size holds");
    }

    #[test]
    fn hidden_entries_are_skipped_and_visibility_sees_the_viewport() {
        let seen: Rc<RefCell<Vec<LayoutViewport>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_by_closure = seen.clone();
        let mut stack = VStack::new();
        stack.push(Box::new(Lines::new(&["always"])));
        stack.push_with(
            Box::new(Lines::new(&["sometimes"])),
            StackEntryOptions::new().with_visible(move |viewport| {
                seen_by_closure.borrow_mut().push(viewport);
                viewport.height >= 8
            }),
        );
        let tall = frame_of(&stack, 20, 10);
        assert_eq!(tall.root.children.len(), 2);
        assert_eq!(tall.root.children[1].rect.y, 1);
        let short = frame_of(&stack, 20, 4);
        assert_eq!(short.root.children.len(), 1, "hidden entry is not laid out");
        assert_eq!(*seen.borrow(), vec![
            LayoutViewport {
                width: 20,
                height: 10
            },
            LayoutViewport {
                width: 20,
                height: 4
            }
        ]);
    }

    #[test]
    fn hstack_allocates_widths_and_alignment() {
        let mut stack = HStack::new().with_gap(1).with_align(StackAlign::Start);
        stack.push(Box::new(Lines::new(&["ab"])));
        stack.push(Box::new(Lines::new(&["cd"])));
        let frame = frame_of(&stack, 10, 4);
        assert_eq!(frame.root.children[0].rect.x, 0);
        assert_eq!(frame.root.children[1].rect.x, 3, "gap is honoured");
        assert_eq!(frame.root.children[0].rect.height, 1, "start keeps height");
        assert!(
            frame.lines[0].starts_with("ab cd"),
            "composited row: {:?}",
            frame.lines[0]
        );
    }

    #[test]
    fn nested_scroll_views_clip_children_and_route_wheel_deltas() {
        // `inner_primary` marks the nested view as the layout's primary view,
        // which is what makes `contain` observable: Pi's routeWheel rule lets an
        // untouched remainder reach the *primary* view unless it was already
        // visited, so with the ancestor as primary the contain and chain rules
        // would look identical.
        fn nested(inner_overscroll: Overscroll, inner_primary: bool) -> Outer {
            let mut body = VStack::new();
            body.push_with(
                Box::new(Lines::rows(20)),
                StackEntryOptions::new().with_basis(StackBasis::Fixed(20)),
            );
            body.push_with(
                Box::new(ScrollView::new(
                    Box::new(Lines::rows(40)),
                    ScrollViewOptions::new()
                        .with_overscroll(inner_overscroll)
                        .with_primary(inner_primary),
                )),
                StackEntryOptions::new()
                    .with_basis(StackBasis::Fixed(4))
                    .with_grow(0)
                    .with_shrink(0),
            );
            Outer {
                view: ScrollView::new(
                    Box::new(body),
                    ScrollViewOptions::new()
                        .with_follow_end(true)
                        .with_primary(true),
                ),
            }
        }

        // Contain: the boundary owns the gesture, so neither the ancestor nor
        // the layout's primary view may consume the delta and the remainder
        // comes back to the caller.
        let outer = nested(Overscroll::Contain, true);
        let frame = frame_of(&outer, 20, 6);
        let views = frame.scroll_views_at(1, 3);
        assert_eq!(views.len(), 2, "both nested viewports contain the point");
        let outer_box = frame.scroll_view_box(views[1]).expect("outer box");
        let inner_box = &outer_box.children[0].children[1];
        assert_eq!(inner_box.rect.height, 4, "the inner viewport is constrained");
        assert!(
            inner_box.clip.height <= outer_box.clip.height,
            "the inner viewport is clipped by the outer viewport"
        );
        assert_eq!(views[0].viewport_height(), 4);
        assert_eq!(views[1].viewport_height(), 6);
        // The inner view reports `contain` and cannot move further up, so the
        // delta never reaches the outer view.
        let remaining = route_wheel_delta(&frame, 1, 3, -2);
        assert_eq!(remaining, -2, "contain stops the chain");
        assert_eq!(views[0].scroll_top(), 0);
        assert_eq!(views[1].scroll_top(), 18, "the outer follow position is untouched");

        // Chain: the untouched remainder is passed outward, so the ancestor
        // consumes it and the caller sees nothing left over.
        let outer = nested(Overscroll::Chain, false);
        let frame = frame_of(&outer, 20, 6);
        let views = frame.scroll_views_at(1, 3);
        let remaining = route_wheel_delta(&frame, 1, 3, -2);
        assert_eq!(remaining, 0, "the outer view consumed the remainder");
        assert_eq!(views[0].scroll_top(), 0, "the inner view starts at its top");
        assert_eq!(views[1].scroll_top(), 16, "the outer view moved by two");
    }

    struct Outer {
        view: ScrollView,
    }

    impl Component for Outer {
        fn render(&self, width: u16) -> Vec<String> {
            self.view.render(width)
        }

        fn invalidate(&mut self) {
            self.view.invalidate();
        }

        fn layout_node(&self) -> Option<LayoutNode<'_>> {
            self.view.layout_node()
        }
    }

    #[test]
    fn chain_passes_the_remainder_up_and_primary_absorbs_the_rest() {
        let primary = ScrollView::new(
            Box::new(Lines::rows(40)),
            ScrollViewOptions::new()
                .with_follow_end(true)
                .with_primary(true),
        );
        let mut stack = VStack::new();
        stack.push_with(
            Box::new(Lines::new(&["header"])),
            StackEntryOptions::new()
                .with_basis(StackBasis::Fixed(2))
                .with_shrink(0),
        );
        stack.push_with(
            Box::new(primary),
            StackEntryOptions::new().with_grow(1),
        );
        let frame = frame_of(&stack, 20, 6);
        let primary = frame.primary_scroll_view.expect("primary view");
        assert_eq!(primary.viewport_height(), 4);
        assert_eq!(primary.scroll_top(), 36, "the primary view follows the end");
        assert!(
            frame.scroll_views_at(1, 0).is_empty(),
            "the pinned header is outside every scroll view"
        );
        let remaining = route_wheel_delta(&frame, 1, 0, -3);
        assert_eq!(remaining, 0);
        assert_eq!(primary.scroll_top(), 33);
        let remaining = route_wheel_delta(&frame, 1, 0, 5);
        assert_eq!(remaining, 2, "only three lines were left below");
        assert!(primary.is_following_end(), "the tail is reached again");
    }

    #[test]
    fn alt_wheel_scrolls_five_times_the_base_step() {
        assert_eq!(wheel_scroll_lines(64, 3), 3, "wheel up without Alt");
        assert_eq!(wheel_scroll_lines(65, 3), 3, "wheel down without Alt");
        assert_eq!(wheel_scroll_lines(64 | 8, 3), 15, "Alt multiplies by five");
        assert_eq!(wheel_scroll_lines(65 | 8, 3), 15);
    }

    #[test]
    fn hit_testing_returns_the_deepest_box_and_respects_clips() {
        let inner = ScrollView::new(Box::new(Lines::rows(30)), ScrollViewOptions::new());
        let mut stack = VStack::new();
        stack.push_with(
            Box::new(Lines::new(&["header"])),
            StackEntryOptions::new()
                .with_basis(StackBasis::Fixed(1))
                .with_grow(0)
                .with_shrink(0),
        );
        stack.push_with(
            Box::new(inner),
            StackEntryOptions::new().with_grow(1),
        );
        let frame = frame_of(&stack, 20, 5);
        let boxes = frame.boxes_at(2, 2);
        assert!(boxes.len() >= 3, "stack, scroll view, and content box");
        assert_eq!(boxes[0].rect.y, 1, "deepest box is the scrolled content");
        assert!(
            frame.boxes_at(2, 4).len() >= 3,
            "the viewport still contains the last row"
        );
        assert!(
            frame.boxes_at(2, 9).is_empty(),
            "rows outside the frame have no boxes"
        );
    }

    #[test]
    fn last_frame_hit_testing_follows_a_resize() {
        let mut stack = VStack::new();
        stack.push_with(
            Box::new(Lines::new(&["top"])),
            StackEntryOptions::new()
                .with_basis(StackBasis::Fixed(1))
                .with_shrink(0),
        );
        stack.push_with(
            Box::new(Lines::rows(20)),
            StackEntryOptions::new().with_grow(1),
        );
        let wide = frame_of(&stack, 80, 24);
        assert_eq!(wide.root.children[1].rect.height, 23);
        let narrow = frame_of(&stack, 40, 8);
        assert_eq!(narrow.root.children[1].rect.height, 7);
        assert!(narrow.boxes_at(30, 6).len() >= 2);
        assert!(
            narrow.boxes_at(70, 6).is_empty(),
            "the old 80-column geometry is not reused"
        );
    }

    #[test]
    fn scrollbar_geometry_tracks_the_scroll_offset() {
        let view = ScrollView::new(
            Box::new(Lines::rows(100)),
            ScrollViewOptions::new().with_scrollbar(Scrollbar::Always),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(view));
        let frame = frame_of(&stack, 20, 10);
        let box_ = frame
            .scroll_view_box(
                frame
                    .scroll_views_at(0, 0)
                    .first()
                    .copied()
                    .expect("scroll view"),
            )
            .expect("box");
        let geometry = scrollbar_geometry(box_, false).expect("always scrollbar");
        assert_eq!(geometry.column, 19);
        assert_eq!(geometry.track_height, 10);
        assert_eq!(geometry.thumb_height, 2, "minimum thumb height");
        assert_eq!(geometry.thumb_top, 0);
        let state = frame.primary_scroll_view.expect("primary");
        state.scroll_to(90, false);
        let frame = frame_of(&stack, 20, 10);
        let box_ = frame
            .scroll_view_box(
                frame
                    .scroll_views_at(0, 0)
                    .first()
                    .copied()
                    .expect("scroll view"),
            )
            .expect("box");
        let geometry = scrollbar_geometry(box_, false).expect("always scrollbar");
        assert_eq!(geometry.thumb_top, 8, "thumb follows the offset");
        assert!(
            frame.lines[0].contains('│'),
            "the track is painted into the viewport: {:?}",
            frame.lines[0]
        );
        assert!(
            frame.lines[8].contains('┃') || frame.lines[8].contains('█'),
            "the thumb is painted at the offset row: {:?}",
            frame.lines[8]
        );
    }

    #[test]
    fn auto_scrollbar_visibility_expires_and_can_be_revealed_for_hit_testing() {
        let view = ScrollView::new(
            Box::new(Lines::rows(50)),
            ScrollViewOptions::new()
                .with_scrollbar(Scrollbar::Auto)
                .with_scrollbar_hide_delay(Duration::from_millis(500)),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(view));
        let frame = frame_of(&stack, 20, 5);
        let state = frame.primary_scroll_view.expect("primary");
        assert!(!state.is_scrollbar_visible(), "idle auto scrollbar is hidden");
        state.scroll_to(10, false);
        assert!(state.is_scrollbar_visible(), "activity reveals it");
        let box_ = frame
            .scroll_view_box(frame.scroll_views_at(0, 0)[0])
            .expect("box");
        assert!(
            scrollbar_geometry(box_, true).is_some(),
            "hidden auto tracks stay hit-testable"
        );
        assert!(
            !state.expire_transient_scrollbar(Instant::now()),
            "the deadline is still ahead"
        );
        assert!(
            state.expire_transient_scrollbar(Instant::now() + Duration::from_secs(2)),
            "the deadline expires the transient visibility"
        );
        assert!(!state.is_scrollbar_visible());
    }

    #[test]
    fn scroll_view_follows_the_end_until_the_reader_moves_away() {
        let view = ScrollView::new(
            Box::new(Lines::rows(30)),
            ScrollViewOptions::new().with_follow_end(true),
        );
        let mut stack = VStack::new();
        stack.push(Box::new(view));
        let frame = frame_of(&stack, 20, 6);
        let state = frame.primary_scroll_view.expect("primary");
        assert!(state.is_following_end());
        assert_eq!(state.scroll_top(), 24);
        state.scroll_by(5);
        assert!(state.is_following_end(), "clamped at the end");
        state.scroll_by(-3);
        assert!(!state.is_following_end(), "moving up leaves follow mode");
        assert_eq!(state.scroll_top(), 21);
        state.scroll_to_end();
        assert!(state.is_following_end());
    }

    #[test]
    fn scroll_view_render_returns_the_full_content_and_reserves_the_scrollbar_column() {
        struct Recording {
            widths: Rc<RefCell<Vec<u16>>>,
        }
        impl Component for Recording {
            fn render(&self, width: u16) -> Vec<String> {
                self.widths.borrow_mut().push(width);
                vec!["content".to_owned()]
            }
            fn invalidate(&mut self) {}
        }
        let widths: Rc<RefCell<Vec<u16>>> = Rc::new(RefCell::new(Vec::new()));
        let view = ScrollView::new(
            Box::new(Recording {
                widths: widths.clone(),
            }),
            ScrollViewOptions::new().with_scrollbar(Scrollbar::Always),
        );
        let lines = view.render(20);
        assert_eq!(*widths.borrow(), vec![19], "one column is reserved");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "content ", "the reserved column is padded");
    }

    #[test]
    fn component_without_a_layout_node_is_a_leaf_even_when_it_contains_one() {
        struct Wrapper {
            inner: VStack,
        }
        impl Component for Wrapper {
            fn render(&self, width: u16) -> Vec<String> {
                self.inner.render(width)
            }
            fn invalidate(&mut self) {
                self.inner.invalidate();
            }
        }
        let mut inner = VStack::new();
        inner.push(Box::new(Lines::new(&["only"])));
        let wrapper = Wrapper { inner };
        let frame = frame_of(&wrapper, 20, 3);
        assert_eq!(frame.root.children.len(), 0, "the wrapper is a leaf");
        assert!(frame.root.lines.is_some());
    }
}

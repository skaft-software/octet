//! Unit tests for `crate::mouse`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mouse.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::mouse`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

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
    let seen: Rc<RefCell<Vec<(TuiMouseEventType, i32, i32)>>> = Rc::new(RefCell::new(Vec::new()));
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

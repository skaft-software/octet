//! Unit tests for `crate::layout`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `layout.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::layout`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

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

fn frame_of(root: &dyn Component, width: u16, height: u16) -> LayoutFrame<'_> {
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
    assert_eq!(
        frame.root.children[0].rect.height, 3,
        "max size caps growth"
    );
    assert_eq!(
        frame.root.children[1].rect.height, 7,
        "grow fills the remainder"
    );
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
    assert_eq!(
        *seen.borrow(),
        vec![
            LayoutViewport {
                width: 20,
                height: 10
            },
            LayoutViewport {
                width: 20,
                height: 4
            }
        ]
    );
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
    assert_eq!(
        inner_box.rect.height, 4,
        "the inner viewport is constrained"
    );
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
    assert_eq!(
        views[1].scroll_top(),
        18,
        "the outer follow position is untouched"
    );

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
    stack.push_with(Box::new(primary), StackEntryOptions::new().with_grow(1));
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
    stack.push_with(Box::new(inner), StackEntryOptions::new().with_grow(1));
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
    assert!(
        !state.is_scrollbar_visible(),
        "idle auto scrollbar is hidden"
    );
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

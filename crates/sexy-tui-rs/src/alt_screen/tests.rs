//! Unit tests for `crate::alt_screen`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `alt_screen.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::alt_screen`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::capabilities::TerminalCapabilities;
use crate::terminal::TerminalInput;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct RecordingTerminal {
    clear_on_write: Rc<Cell<bool>>,
    size: (u16, u16),
    writes: Rc<RefCell<String>>,
    capabilities: TerminalCapabilities,
}

impl Terminal for RecordingTerminal {
    fn start_events(
        &mut self,
        _on_input: Box<dyn FnMut(TerminalInput)>,
        _on_resize: Box<dyn FnMut()>,
    ) {
    }

    fn stop(&mut self) {}

    fn write(&mut self, data: &str) {
        let mut buffer = self.writes.borrow_mut();
        if self.clear_on_write.get() {
            buffer.clear();
        }
        buffer.push_str(data);
    }

    fn columns(&self) -> u16 {
        self.size.0
    }

    fn rows(&self) -> u16 {
        self.size.1
    }

    fn move_by(&mut self, _lines: i16) {}

    fn hide_cursor(&mut self) {}

    fn show_cursor(&mut self) {}

    fn clear_line(&mut self) {}

    fn clear_from_cursor(&mut self) {}

    fn clear_screen(&mut self) {}

    fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }
}

fn recording(size: (u16, u16)) -> (RecordingTerminal, Rc<RefCell<String>>, Rc<Cell<bool>>) {
    let writes: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let clear_on_write = Rc::new(Cell::new(false));
    (
        RecordingTerminal {
            clear_on_write: clear_on_write.clone(),
            size,
            writes: writes.clone(),
            capabilities: TerminalCapabilities::interactive(crate::ColorDepth::None, true),
        },
        writes,
        clear_on_write,
    )
}

fn options(synchronized_output: bool) -> AltScreenOptions {
    AltScreenOptions {
        mouse: false,
        all_motion_mouse: false,
        synchronized_output,
    }
}

#[test]
fn enter_disables_autowrap_and_clears_the_fixed_viewport() {
    let (mut terminal, writes, _) = recording((80, 24));
    let mut session = AlternateScreen::new(options(false));
    assert!(session.enter(&mut terminal));
    assert_eq!(
        writes.borrow().as_str(),
        "\x1b[?1049h\x1b[?7l\x1b[2J\x1b[H\x1b[?25l"
    );
    assert!(session.is_active());
}

#[test]
fn enter_refuses_a_terminal_without_cursor_addressing() {
    let (mut terminal, writes, _) = recording((80, 24));
    let mut capabilities = terminal.capabilities;
    capabilities.cursor_addressing = false;
    terminal.capabilities = capabilities;
    let mut session = AlternateScreen::new(options(false));
    assert!(!session.enter(&mut terminal));
    assert!(writes.borrow().is_empty());
    assert!(!session.is_active());
}

#[test]
fn paint_addresses_every_row_absolutely_and_crops_to_width() {
    let (mut terminal, writes, clear) = recording((10, 4));
    let mut session = AlternateScreen::new(options(false));
    assert!(session.enter(&mut terminal));
    clear.set(true);
    let frame = vec![
        "0123456789ABCDEF".to_owned(),
        "second".to_owned(),
        "third".to_owned(),
        "fourth".to_owned(),
    ];
    session.paint(&mut terminal, &frame, 10, 4, None, false);
    let output = writes.borrow().clone();
    assert_eq!(
        output,
        "\x1b[2J\x1b[1;1H\x1b[2K0123456789\x1b[2;1H\x1b[2Ksecond\x1b[3;1H\x1b[2Kthird\x1b[4;1H\x1b[2Kfourth\x1b[?25l",
        "{output:?}"
    );
    assert_eq!(crop_to_width("0123456789ABCDEF", 10), "0123456789");

    // The next frame only repaints changed rows and keeps the window.
    clear.set(true);
    let mut next = frame.clone();
    next[2] = "THIRD".to_owned();
    session.paint(&mut terminal, &next, 10, 4, None, false);
    let output = writes.borrow().clone();
    assert_eq!(output, "\x1b[3;1H\x1b[2KTHIRD\x1b[?25l", "{output:?}");
    assert_eq!(session.window()[2], "THIRD");
}

#[test]
fn paint_uses_the_tail_window_and_places_the_hardware_cursor() {
    let (mut terminal, writes, clear) = recording((10, 2));
    let mut session = AlternateScreen::new(options(false));
    assert!(session.enter(&mut terminal));
    clear.set(true);
    let document = vec!["older".to_owned(), "middle".to_owned(), "newest".to_owned()];
    session.paint(&mut terminal, &document, 10, 2, Some((1, 3)), true);
    let output = writes.borrow().clone();
    assert!(output.contains("\x1b[1;1H\x1b[2Kmiddle"), "{output:?}");
    assert!(output.contains("\x1b[2;1H\x1b[2Knewest"), "{output:?}");
    assert!(output.contains("\x1b[2;4H\x1b[?25h"), "{output:?}");
}

#[test]
fn exit_restores_the_final_document_to_the_main_screen() {
    let (mut terminal, writes, clear) = recording((10, 4));
    let mut session = AlternateScreen::new(options(false));
    assert!(session.enter(&mut terminal));
    let document = vec![
        "first".to_owned(),
        "0123456789ABCDEF".to_owned(),
        "third".to_owned(),
    ];
    clear.set(true);
    session.exit(&mut terminal, &document, false);
    assert_eq!(
        writes.borrow().as_str(),
        "\x1b[?1049l\x1b[?7l\r\x1b[2Kfirst\r\n\r\x1b[2K0123456789\r\n\r\x1b[2Kthird\x1b[0m\x1b[?7h\r\n\x1b[?25h",
        "{:?}",
        writes.borrow()
    );
    assert!(!session.is_active());
}

#[test]
fn exit_without_the_transcript_only_restores_the_main_screen() {
    let (mut terminal, writes, clear) = recording((10, 4));
    let mut session = AlternateScreen::new(options(false));
    assert!(session.enter(&mut terminal));
    clear.set(true);
    session.exit(&mut terminal, &["ignored".to_owned()], true);
    assert_eq!(writes.borrow().as_str(), "\x1b[?1049l\x1b[?25h");
}

#[test]
fn synchronized_output_wraps_enter_frames_and_exit() {
    let (mut terminal, writes, _) = recording((20, 3));
    let mut session = AlternateScreen::new(options(true));
    assert!(session.enter(&mut terminal));
    assert!(writes.borrow().ends_with("\x1b[?25l"));
    let mut writes_guard = writes.borrow_mut();
    writes_guard.clear();
    drop(writes_guard);
    session.paint(
        &mut terminal,
        &["row".to_owned(), "two".to_owned(), "three".to_owned()],
        20,
        3,
        None,
        false,
    );
    assert!(writes.borrow().starts_with(BEGIN_SYNCHRONIZED_OUTPUT));
    assert!(writes.borrow().ends_with(END_SYNCHRONIZED_OUTPUT));
    let mut writes_guard = writes.borrow_mut();
    writes_guard.clear();
    drop(writes_guard);
    session.exit(&mut terminal, &["done".to_owned()], false);
    let output = writes.borrow().clone();
    assert!(output.starts_with(BEGIN_SYNCHRONIZED_OUTPUT), "{output:?}");
    assert!(output.ends_with(END_SYNCHRONIZED_OUTPUT), "{output:?}");
    assert!(output.contains(ENABLE_AUTOWRAP), "{output:?}");
}

#[test]
fn a_paint_before_enter_is_a_no_op() {
    let (mut terminal, writes, _) = recording((20, 3));
    let mut session = AlternateScreen::new(options(false));
    session.paint(&mut terminal, &["row".to_owned()], 20, 3, None, false);
    assert!(writes.borrow().is_empty());
}

/// The internal anchor a retained renderer puts on the row a Kitty image starts.
fn anchor(id: u32, rows: u16) -> String {
    use crate::images::{ImageId, ImageLayout};
    ImageAnchor::new(
        ImageProtocol::Kitty,
        ImageId::new(id).unwrap(),
        ImageLayout::new(20, rows).unwrap(),
    )
    .marker()
}

fn delete(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// A session that has painted `frame` once, plus the writes of later paints.
fn painted(
    frame: &[String],
    size: (u16, u16),
) -> (
    AlternateScreen,
    RecordingTerminal,
    Rc<RefCell<String>>,
    Rc<Cell<bool>>,
) {
    let (mut terminal, writes, clear) = recording(size);
    let mut session = AlternateScreen::new(options(false));
    assert!(session.enter(&mut terminal));
    session.paint(&mut terminal, frame, size.0, size.1, None, false);
    clear.set(true);
    (session, terminal, writes, clear)
}

fn rows(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

#[test]
fn a_scrolled_image_is_deleted_before_it_is_placed_at_its_new_row() {
    let image = anchor(7, 3);
    let before = rows(&["header", &image, "", "", "tail", "end"]);
    let (mut session, mut terminal, writes, _) = painted(&before, (40, 6));

    // PgUp/PgDn: every row moves, and the image starts one row lower.
    let after = rows(&["", "header", &image, "", "", "tail"]);
    session.paint(&mut terminal, &after, 40, 6, None, false);
    let output = writes.borrow().clone();

    let deleted = output
        .find(&delete(7))
        .expect("the old placement is deleted");
    let placed = output.find(&image).expect("the image is placed again");
    assert!(
        deleted < placed,
        "delete must precede placement: {output:?}"
    );
    assert!(
        output.contains(&format!("\x1b[3;1H\x1b[2K{image}")),
        "the image is placed at its new row: {output:?}"
    );
    assert_eq!(output.matches(&image).count(), 1, "{output:?}");
}

#[test]
fn a_status_tick_away_from_the_image_neither_deletes_nor_replaces_it() {
    let image = anchor(7, 3);
    let before = rows(&["header", &image, "", "", "tail", "clock 1"]);
    let (mut session, mut terminal, writes, _) = painted(&before, (40, 6));

    let after = rows(&["header", &image, "", "", "tail", "clock 2"]);
    session.paint(&mut terminal, &after, 40, 6, None, false);
    let output = writes.borrow().clone();

    assert!(!output.contains("\x1b_G"), "{output:?}");
    assert!(!output.contains(&image), "{output:?}");
    assert!(output.contains("clock 2"), "{output:?}");
}

#[test]
fn changing_a_row_an_image_covers_replaces_that_image() {
    let image = anchor(7, 3);
    let before = rows(&["header", &image, "", "", "tail", "end"]);
    let (mut session, mut terminal, writes, _) = painted(&before, (40, 6));

    // Row 3 is one of the image's reserved rows; text now lands under it.
    let after = rows(&["header", &image, "", "overlay", "tail", "end"]);
    session.paint(&mut terminal, &after, 40, 6, None, false);
    let output = writes.borrow().clone();

    assert!(output.contains(&delete(7)), "{output:?}");
    assert!(output.contains(&image), "{output:?}");
}

#[test]
fn an_image_scrolled_out_of_the_window_is_deleted_and_not_replaced() {
    let image = anchor(7, 3);
    let before = rows(&["header", &image, "", "", "tail", "end"]);
    let (mut session, mut terminal, writes, _) = painted(&before, (40, 6));

    let after = rows(&["", "", "tail", "end", "more", "last"]);
    session.paint(&mut terminal, &after, 40, 6, None, false);
    let output = writes.borrow().clone();

    assert!(output.contains(&delete(7)), "{output:?}");
    assert!(!output.contains(&image), "{output:?}");
}

#[test]
fn deleting_an_image_id_replaces_every_visible_placement_of_that_id() {
    // The same screenshot read twice shares one image id. Deleting it removes
    // both placements, so the one that did not move must be placed again.
    let image = anchor(7, 2);
    let before = rows(&[&image, "", "middle", &image, "", "end"]);
    let (mut session, mut terminal, writes, _) = painted(&before, (40, 6));

    let after = rows(&[&image, "", "middle", "text", "more", "end"]);
    session.paint(&mut terminal, &after, 40, 6, None, false);
    let output = writes.borrow().clone();

    assert!(output.contains(&delete(7)), "{output:?}");
    assert_eq!(output.matches(&image).count(), 1, "{output:?}");
    assert!(
        output.contains(&format!("\x1b[1;1H\x1b[2K{image}")),
        "the unmoved placement is placed again: {output:?}"
    );
}

#[test]
fn a_full_redraw_deletes_the_images_it_is_about_to_repaint() {
    let image = anchor(7, 3);
    let before = rows(&["header", &image, "", "", "tail", "end"]);
    let (mut session, mut terminal, writes, _) = painted(&before, (40, 6));

    // A resize repaints every row; the old placement is at stale cells.
    session.paint(&mut terminal, &before, 30, 6, None, false);
    let output = writes.borrow().clone();

    let deleted = output
        .find(&delete(7))
        .expect("the old placement is deleted");
    let placed = output.find(&image).expect("the image is placed again");
    assert!(deleted < placed, "{output:?}");

    // `invalidate` forces the same full redraw and must not forget the image.
    clear_writes(&writes);
    session.invalidate();
    session.paint(&mut terminal, &before, 30, 6, None, false);
    assert!(
        writes.borrow().contains(&delete(7)),
        "{:?}",
        writes.borrow()
    );
}

#[test]
fn leaving_the_alternate_screen_deletes_placed_images() {
    let image = anchor(7, 3);
    let before = rows(&["header", &image, "", "", "tail", "end"]);
    let (mut session, mut terminal, writes, _) = painted(&before, (40, 6));

    session.exit(&mut terminal, &before, true);
    let output = writes.borrow().clone();
    assert!(output.contains(&delete(7)), "{output:?}");
    assert!(
        output.find(&delete(7)) < output.find(EXIT_ALT_SCREEN),
        "{output:?}"
    );
}

fn clear_writes(writes: &Rc<RefCell<String>>) {
    writes.borrow_mut().clear();
}

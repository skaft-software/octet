//! Real output adapter with an in-memory sink; no physical terminal lifecycle.
use super::*;
use sexy_tui_rs::{Terminal, TerminalCapabilities, TerminalInput};

#[path = "../../../../sexy-tui-rs/tests/support/kitty.rs"]
pub(crate) mod model;

pub(crate) type Frames = Arc<Mutex<Vec<Vec<u8>>>>;

struct Sink(Frames);
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl FrameSink for Sink {}

struct GraphicsTerminal(OctetTerminal<Sink>);
impl Terminal for GraphicsTerminal {
    fn start_events(&mut self, _: Box<dyn FnMut(TerminalInput)>, _: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn write(&mut self, data: &str) {
        self.0.write(data);
    }
    fn columns(&self) -> u16 {
        self.0.columns()
    }
    fn rows(&self) -> u16 {
        self.0.rows()
    }
    fn move_by(&mut self, rows: i16) {
        self.0.move_by(rows);
    }
    fn hide_cursor(&mut self) {
        self.0.hide_cursor();
    }
    fn show_cursor(&mut self) {
        self.0.show_cursor();
    }
    fn clear_line(&mut self) {
        self.0.clear_line();
    }
    fn clear_from_cursor(&mut self) {
        self.0.clear_from_cursor();
    }
    fn clear_screen(&mut self) {
        self.0.clear_screen();
    }
    fn capabilities(&self) -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::interactive(sexy_tui_rs::ColorDepth::TrueColor, true);
        caps.synchronized_output = true;
        caps.sync_output = true;
        caps
    }
}

pub(crate) fn terminal(
    size: TerminalSize,
    image_store: TerminalImageStore,
) -> (impl Terminal, Frames) {
    graphics_terminal(size, image_store)
}

fn graphics_terminal(
    size: TerminalSize,
    image_store: TerminalImageStore,
) -> (GraphicsTerminal, Frames) {
    let frames = Frames::default();
    (
        GraphicsTerminal(OctetTerminal {
            out: Sink(frames.clone()),
            size,
            last_was_cr: false,
            hyperlink_filter: None,
            pending: Vec::new(),
            pending_log: Vec::new(),
            image_store,
            write_log: None,
            in_synchronized_frame_depth: 0,
            initial_frame: false,
            held: HeldImages::default(),
        }),
        frames,
    )
}

#[test]
fn kitty_graphics_backend_preserves_cropped_and_repeated_ids_across_frames() {
    use sexy_tui_rs::{AltScreenOptions, AlternateScreen, ImageLayout};
    let store = TerminalImageStore::default();
    let id = ImageId::new(7).unwrap();
    store.register(
        id,
        Arc::new(
            TerminalImage::from_bytes(include_bytes!("fixtures/screenshot.png").to_vec()).unwrap(),
        ),
    );
    let (mut terminal, frames) = terminal(Arc::new(Mutex::new((40, 10))), store);
    let mut screen = AlternateScreen::new(AltScreenOptions {
        synchronized_output: true,
        ..AltScreenOptions::default()
    });
    let mut model = model::Kitty::new(10, 40);
    screen.enter(&mut terminal);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    let anchor = ImageAnchor::new(ImageProtocol::Kitty, id, ImageLayout::new(20, 4).unwrap());
    let mut document = vec![String::new(); 18];
    document[1] = format!("  {}", anchor.marker());
    document[6] = "Read screenshot again".into();
    document[9] = format!("  {}", anchor.marker());
    document[14] = "Read next.png".into();
    for (index, start) in (0..14).chain((0..14).rev()).enumerate() {
        let mut window = sexy_tui_rs::images::crop_anchors_to_window(&document, start, start + 8);
        window.resize(8, String::new());
        window.push(format!("↑ rows back · {index} new"));
        window.push("composer / footer".into());
        if index % 7 == 0 {
            screen.invalidate();
        }
        screen.paint(&mut terminal, &window, 40, 10, None, false);
        let expected: Vec<_> = window
            .iter()
            .enumerate()
            .flat_map(|(row, line)| {
                ImageAnchor::parse_all(line)
                    .into_iter()
                    .map(move |anchor| model::Placement {
                        image: anchor.id().get(),
                        placement: None,
                        row: row as u16,
                        column: anchor.column().unwrap_or(2),
                        rows: anchor.placed_rows(),
                        columns: anchor.layout().columns(),
                    })
            })
            .collect();
        for frame in frames.lock().unwrap().drain(..) {
            model.feed(&frame);
            model.assert_placements(&expected);
        }
    }
    screen.exit(&mut terminal, &[], true);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    model.assert_placements(&[]);
}

// Unix-only: the locked Pi reference fixture is installed by the Linux and macOS
// CI lanes; the Windows lane runs native terminal tests without it.
#[cfg(unix)]
#[test]
fn kitty_graphics_pi_reference_raw_rows_share_fullscreen_lifecycle() {
    use sexy_tui_rs::{AltScreenOptions, AlternateScreen};
    // Execute the locked test reference, not a hand-written encoder. Source
    // contract: packages/tui/src/terminal-image.ts:237-278 (encodeKitty).
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "-e", r#"
import { encodeKitty } from './node_modules/pi-session-1-5-0/node_modules/@earendil-works/pi-tui/dist/terminal-image.js';
process.stdout.write(encodeKitty('AA==', {imageId: 7, columns: 20, rows: 4, moveCursor: false}));
"#])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fixtures/pi-reference"))
        .output().expect("locked Pi reference requires Node");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = String::from_utf8(output.stdout).unwrap();
    assert!(raw.starts_with("\x1b_Ga=T,"));
    let (mut terminal, frames) = terminal(
        Arc::new(Mutex::new((40, 10))),
        TerminalImageStore::default(),
    );
    let mut screen = AlternateScreen::new(AltScreenOptions {
        synchronized_output: true,
        ..AltScreenOptions::default()
    });
    let mut model = model::Kitty::new(10, 40);
    screen.enter(&mut terminal);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    for (index, row) in [2, 3, 3, 0, 5, 1, 0].into_iter().enumerate() {
        let mut window = vec![String::new(); 10];
        window[row] = raw.clone();
        window[9] = format!("↑ 199 rows back · {index} new");
        screen.paint(&mut terminal, &window, 40, 10, None, false);
        for frame in frames.lock().unwrap().drain(..) {
            model.feed(&frame);
            model.assert_placements(&[model::Placement {
                image: 7,
                placement: None,
                row: row as u16,
                column: 0,
                rows: 4,
                columns: 20,
            }]);
        }
    }
    screen.exit(&mut terminal, &[], true);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    model.assert_placements(&[]);
}

// A scroll moves placements; it must not re-send image data. Re-transmitting
// and re-decoding every visible screenshot on every frame is what made fast
// fullscreen scrolling fall behind in libghostty (Rex), leaving stale images.
#[test]
fn kitty_graphics_scroll_places_held_images_without_resending_data() {
    use sexy_tui_rs::{AltScreenOptions, AlternateScreen, ImageLayout};
    let store = TerminalImageStore::default();
    let png = || {
        Arc::new(
            TerminalImage::from_bytes(include_bytes!("fixtures/screenshot.png").to_vec()).unwrap(),
        )
    };
    let first = ImageId::new(21).unwrap();
    let second = ImageId::new(22).unwrap();
    store.register(first, png());
    store.register(second, png());
    let (mut terminal, frames) = terminal(Arc::new(Mutex::new((40, 10))), store);
    let mut screen = AlternateScreen::new(AltScreenOptions {
        synchronized_output: true,
        ..AltScreenOptions::default()
    });
    let mut model = model::Kitty::new(10, 40);
    screen.enter(&mut terminal);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    let layout = ImageLayout::new(20, 4).unwrap();
    let mut document = vec![String::new(); 30];
    document[2] = format!(
        "  {}",
        ImageAnchor::new(ImageProtocol::Kitty, first, layout).marker()
    );
    document[8] = "window state".into();
    document[12] = format!(
        "  {}",
        ImageAnchor::new(ImageProtocol::Kitty, second, layout).marker()
    );
    document[18] = "Click  computer use click".into();
    let mut previous: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    // Scroll down and back up one row per frame, like a wheel burst.
    for start in (0..22).chain((0..22).rev()) {
        let mut window = sexy_tui_rs::images::crop_anchors_to_window(&document, start, start + 8);
        window.resize(8, String::new());
        window.push("[ ↓ Jump ]".into());
        window.push("composer / footer".into());
        screen.paint(&mut terminal, &window, 40, 10, None, false);
        let visible: Vec<_> = window
            .iter()
            .enumerate()
            .flat_map(|(row, line)| {
                ImageAnchor::parse_all(line)
                    .into_iter()
                    .map(move |anchor| (row, anchor))
            })
            .collect();
        let expected: Vec<_> = visible
            .iter()
            .map(|(row, anchor)| model::Placement {
                image: anchor.id().get(),
                placement: None,
                row: *row as u16,
                column: anchor.column().unwrap_or(2),
                rows: anchor.placed_rows(),
                columns: anchor.layout().columns(),
            })
            .collect();
        let ids: std::collections::BTreeSet<u32> = visible
            .iter()
            .map(|(_, anchor)| anchor.id().get())
            .collect();
        // Data is sent when an image enters the viewport; an image that stays
        // on screen is only placed again by ID.
        let entering = ids.difference(&previous).count();
        let before = model.transmissions;
        for frame in frames.lock().unwrap().drain(..) {
            model.feed(&frame);
        }
        model.assert_placements(&expected);
        assert_eq!(
            model.transmissions - before,
            entering,
            "frame at row {start}: only images entering the viewport send data (visible {ids:?}, previous {previous:?})"
        );
        previous = ids;
    }
    // A full redraw may free image data, so every visible image is sent again
    // and none is placed by ID (the model rejects a=p without data).
    let mut window = sexy_tui_rs::images::crop_anchors_to_window(&document, 0, 8);
    window.resize(8, String::new());
    window.push("[ ↓ Jump ]".into());
    window.push("composer / footer".into());
    let visible = window
        .iter()
        .flat_map(|line| ImageAnchor::parse_all(line))
        .map(|anchor| anchor.id().get())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    screen.invalidate();
    let before = model.transmissions;
    screen.paint(&mut terminal, &window, 40, 10, None, false);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    assert_eq!(
        model.transmissions - before,
        visible,
        "full redraw resends visible images"
    );
    screen.exit(&mut terminal, &[], true);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    model.assert_placements(&[]);
}

// More images than any off-screen cache scrolled out and back. A data-freeing
// delete (d=I) makes kitty free every image left without a placement, so an
// image whose data was kept off screen can be gone when it scrolls back; placing
// it by ID would leave it blank (q=2 hides the error). Every re-entry must send
// data, and frames that only move or crop resident images must send none.
#[test]
fn kitty_graphics_scroll_past_many_images_never_places_freed_data() {
    use sexy_tui_rs::{AltScreenOptions, AlternateScreen, ImageLayout};
    const IMAGES: usize = 18;
    let store = TerminalImageStore::default();
    let layout = ImageLayout::new(20, 4).unwrap();
    let mut document = vec![String::new(); IMAGES * 6 + 8];
    for index in 0..IMAGES {
        let id = ImageId::new(100 + index as u32).unwrap();
        store.register(
            id,
            Arc::new(
                TerminalImage::from_bytes(include_bytes!("fixtures/screenshot.png").to_vec())
                    .unwrap(),
            ),
        );
        document[index * 6 + 1] = format!(
            "  {}",
            ImageAnchor::new(ImageProtocol::Kitty, id, layout).marker()
        );
        document[index * 6 + 5] = format!("caption {index}");
    }
    let (mut terminal, frames) = terminal(Arc::new(Mutex::new((40, 10))), store);
    let mut screen = AlternateScreen::new(AltScreenOptions {
        synchronized_output: true,
        ..AltScreenOptions::default()
    });
    let mut model = model::Kitty::new(10, 40);
    screen.enter(&mut terminal);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    let last = document.len() - 8;
    let mut previous = std::collections::BTreeSet::new();
    for start in (0..=last).chain((0..=last).rev()).chain(0..=last) {
        let mut window = sexy_tui_rs::images::crop_anchors_to_window(&document, start, start + 8);
        window.resize(8, String::new());
        window.push("composer".into());
        window.push("footer".into());
        screen.paint(&mut terminal, &window, 40, 10, None, false);
        let visible: std::collections::BTreeSet<u32> = window
            .iter()
            .flat_map(|line| ImageAnchor::parse_all(line))
            .map(|anchor| anchor.id().get())
            .collect();
        let entering = visible.difference(&previous).count();
        let (transmissions, bytes) = (model.transmissions, model.payload_bytes);
        for frame in frames.lock().unwrap().drain(..) {
            model.feed(&frame);
        }
        assert_eq!(
            model.transmissions - transmissions,
            entering,
            "row {start}: only images entering the viewport send data"
        );
        if entering == 0 {
            assert_eq!(
                model.payload_bytes, bytes,
                "row {start}: moving or cropping resident images retransmits no payload"
            );
        }
        previous = visible;
    }
    screen.exit(&mut terminal, &[], true);
    for frame in frames.lock().unwrap().drain(..) {
        model.feed(&frame);
    }
    model.assert_placements(&[]);
}

// Held IDs are bookkeeping for images still registered: releasing images must
// shrink it, however many images a long session shows.
#[test]
fn held_image_ids_stay_bounded_by_live_images() {
    use sexy_tui_rs::ImageLayout;
    let store = TerminalImageStore::default();
    let (mut terminal, _frames) = graphics_terminal(Arc::new(Mutex::new((40, 10))), store.clone());
    let layout = ImageLayout::new(4, 2).unwrap();
    for n in 1..=50 {
        let id = ImageId::new(n).unwrap();
        store.register(
            id,
            Arc::new(
                TerminalImage::from_bytes(include_bytes!("fixtures/screenshot.png").to_vec())
                    .unwrap(),
            ),
        );
        terminal.write(&format!(
            "\x1b[1;1H{}",
            ImageAnchor::new(ImageProtocol::Kitty, id, layout).marker()
        ));
        if n > 3 {
            store.release(ImageId::new(n - 3).unwrap());
        }
        let held = terminal.0.held.confirmed.len();
        assert!(held <= 4, "{held} held IDs after {n} images, 4 live");
    }
}

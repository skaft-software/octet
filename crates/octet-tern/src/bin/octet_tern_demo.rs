//! Render a representative octet session as native Tern surfaces.
//!
//! Run inside a Tern pane:
//!
//! ```console
//! cargo run -p octet-tern --bin octet-tern-demo -- --theme examples/themes/Cards.toml
//! ```
//!
//! Outside Tern it exits with a note; octet keeps its ANSI path there.

use std::process::ExitCode;

use octet_tern::client::{self, TernClient};
use octet_tern::scene;
use octet_tern::theme;
use octet_tern::wire::SurfaceMode;

/// The surface id shared by every frame this demo sends.
const SURFACE: &str = "octet.session";

fn main() -> ExitCode {
    if !client::is_tern() {
        eprintln!("octet-tern-demo: not inside a Tern pane (TERM_PROGRAM=tern); nothing to draw.");
        return ExitCode::from(2);
    }

    let mut theme_path: Option<String> = None;
    let mut hold_seconds: u64 = 30;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--theme" => theme_path = args.next(),
            "--hold" => {
                hold_seconds = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(hold_seconds);
            }
            other => {
                eprintln!("octet-tern-demo: unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    let default_theme = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/themes/Cards.toml"
    );
    let path = theme_path.as_deref().unwrap_or(default_theme);
    let palette = match theme::load(path) {
        Ok(palette) => palette,
        Err(error) => {
            eprintln!("octet-tern-demo: {path}: {error}; using the built-in palette");
            theme::OctetPalette::default()
        }
    };

    if let Err(error) = run(&palette, hold_seconds) {
        eprintln!("octet-tern-demo: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run(palette: &theme::OctetPalette, hold_seconds: u64) -> std::io::Result<()> {
    let mut tern = TernClient::connect("octet", Some(env!("CARGO_PKG_VERSION")))?;
    tern.open(SURFACE, SurfaceMode::Inline, "octet", Some("octet.session"))?;
    tern.palette(&palette.to_wire(SURFACE))?;

    let ops = scene::demo_session(SURFACE);
    tern.frame_ops(SURFACE, ops)?;

    // Stay alive long enough to be seen (and captured), draining acks, resizes
    // and theme changes so flow control keeps working.
    let mut waited = 0u64;
    while waited < hold_seconds * 1000 {
        let _ = tern.read_one(500);
        waited += 500;
    }

    tern.close(SURFACE, true)?;
    Ok(())
}

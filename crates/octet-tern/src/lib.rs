#![deny(missing_docs)]
#![deny(unsafe_code)]

//! Wire types and a client for the **Tern Surface Protocol** (TSP).
//!
//! Tern — Stencil's native multiplexing terminal — draws program UI itself when
//! the program describes that UI as semantic data. Programs send TSP frames over
//! their own tty as APC sequences (`ESC _ tsp;… ESC \`); the terminal reconciles
//! them into native surfaces instead of consuming ANSI art. TSP is what makes
//! `omp` look like an application rather than a character grid, and these types
//! let octet speak the same protocol.
//!
//! The vocabulary here mirrors TSP v1 exactly (`TSP_VERSION`): the same verbs,
//! component kinds, tones, ops, node shape, theme palette and events. A program
//! that speaks this module is indistinguishable from any other TSP client,
//! including `omp`.
//!
//! # Layout
//!
//! - [`wire`] — the message schema (verbs, kinds, ops, nodes, palette, events).
//! - [`frame`] — APC framing, UTF-8-safe chunking, and reply/event decoding.
//! - [`client`] — Tern detection and surface lifecycle over the tty.
//! - [`theme`] — projection of an octet theme into a TSP theme palette.
//! - [`scene`] — builders for octet's coding surfaces (transcript, tools,
//!   working row, todos, composer).
//!
//! # Example
//!
//! ```no_run
//! use octet_tern::{client::TernClient, scene, wire::SurfaceMode};
//!
//! let mut client = TernClient::connect("octet", None)?;
//! client.open("octet.session", SurfaceMode::Inline, "octet", Some("octet.session"))?;
//! client.frame_ops("octet.session", scene::demo_session("octet.session"))?;
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod client;
pub mod frame;
pub mod reconcile;
pub mod scene;
pub mod theme;
mod tty;
pub mod wire;

pub use wire::{TSP_DEFAULT_APC_LIMIT, TSP_DEFAULT_CREDITS, TSP_KINDS, TSP_PREFIX, TSP_VERSION};

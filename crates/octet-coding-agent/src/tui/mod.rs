#![allow(missing_docs)]

pub mod composer;
pub mod composer_surface;
pub(crate) mod context;
// Host-owned cache used by negotiated API 0.4 remote components.
pub(crate) mod extension_components;
pub(crate) mod fuzzy;
pub mod keymap;
pub(crate) mod layout;
pub mod pickers;
pub(crate) mod splash;
pub mod terminal;
pub mod theme;
// Theme watcher conformance model; production uses crate::reload.
#[cfg(test)]
pub mod theme_reload;
mod theme_schema;
pub mod view;

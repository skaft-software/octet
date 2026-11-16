#![allow(missing_docs)]
//! Frontend presentation for the shared Codex provider.
mod login;
pub use login::{login, login_without_prompt, logout};
pub(crate) use login::{login_progress_channel, login_with_progress};
pub use octet_ai::provider_auth::codex::*;

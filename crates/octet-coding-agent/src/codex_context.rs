//! Frontend Codex context presentation and launch-only environment publication.
#![allow(missing_docs)]
pub use octet_ai::providers::codex_context::*;

pub fn publish(override_: CodexContextOverride) {
    match override_.requested_tokens {
        Some(tokens) => std::env::set_var(CODEX_CONTEXT_OVERRIDE_ENV, tokens.to_string()),
        None => std::env::remove_var(CODEX_CONTEXT_OVERRIDE_ENV),
    }
}

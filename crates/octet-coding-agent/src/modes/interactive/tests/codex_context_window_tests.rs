//! Context-window controls reflect the effective Codex route, without old
//! surcharge/transport warnings or subscription-price claims.
use super::support::*;
use super::*;

#[test]
fn codex_context_surface_follows_the_declared_route_and_the_effective_window() {
    assert!(codex_context_surface(&scripted_model("http://127.0.0.1:1")).is_none());
    let mut codex = scripted_codex_model("http://127.0.0.1:1");
    let spec = std::sync::Arc::make_mut(&mut codex.spec);
    spec.id = octet_ai::ModelId("codex/gpt-6.1-sol".into());
    spec.api_name = "gpt-6.1-sol".into();
    spec.limits.context_window = 500_000;
    spec.context_window_options = Some(octet_ai::types::ContextWindowOptions {
        default: 500_000,
        maximum: 600_000,
    });
    let surface = codex_context_surface(&codex).unwrap();
    assert_eq!(surface.effective_window(), 500_000);
    assert_eq!(surface.raise_target(), Some(600_000));
    assert!(pickers::codex_context_menu_row(&surface).contains("500000"));
    let lines = surface.summary_lines().join("\n");
    assert!(lines.contains("effective 500K"));
    assert!(lines.contains("maximum 600K"));
    for obsolete in [
        "UNCERTAIN",
        "double-priced",
        "websocket",
        "acknowledge",
        "Pro or ProLite",
    ] {
        assert!(!lines.contains(obsolete), "{lines}");
    }
}

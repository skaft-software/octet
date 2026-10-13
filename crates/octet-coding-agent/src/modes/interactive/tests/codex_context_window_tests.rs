//! The Codex context-window surface: which routes offer the effort menu and what
//! effective window they report.
//! Separate because it is the only probe that couples a menu's contents to a declared
//! provider capability.

use super::*;

use super::support::*;

#[test]
fn codex_context_surface_follows_the_declared_route_and_the_effective_window() {
    let plain = scripted_model("http://127.0.0.1:1");
    assert!(
        codex_context_surface(&plain).is_none(),
        "a non-Codex route must not offer a Codex context-window surface"
    );

    let mut codex = scripted_codex_model("http://127.0.0.1:1");
    std::sync::Arc::make_mut(&mut codex.spec).id = octet_ai::ModelId("gpt-6-astra".into());
    std::sync::Arc::make_mut(&mut codex.spec)
        .limits
        .context_window = 272_000;
    let surface = codex_context_surface(&codex).expect("Codex route");
    assert_eq!(surface.effective_window(), 272_000);
    assert!(!surface.has_uncertain_usage());
    let row = pickers::codex_context_menu_row(&surface);
    assert!(row.contains("272000"), "{row}");
    let lines = surface.summary_lines().join("\n");
    assert!(lines.contains("272K"), "{lines}");
    assert!(
        lines.contains("effective 272K"),
        "the surface must label the effective window it reports: {lines}"
    );

    // An above-standard-tier route renders its accounting as uncertain and
    // never as an exact figure.
    std::sync::Arc::make_mut(&mut codex.spec).id = octet_ai::ModelId("gpt-5.6-luna".into());
    std::sync::Arc::make_mut(&mut codex.spec)
        .limits
        .context_window = 372_000;
    let uncertain = codex_context_surface(&codex).expect("Codex route");
    assert!(uncertain.has_uncertain_usage());
    assert!(pickers::codex_context_menu_row(&uncertain).contains("UNCERTAIN"));
    let lines = uncertain.summary_lines().join("\n");
    assert!(lines.contains("UNCERTAIN"), "{lines}");
    assert!(lines.contains("double-priced"), "{lines}");
    assert!(
        !lines.contains(octet_ai_operation_name()),
        "the internal operation id must never be rendered: {lines}"
    );
    assert!(!lines.contains("::"), "{lines}");
    assert!(
        !lines.contains('$'),
        "no exact-looking figure may be rendered above the standard tier: {lines}"
    );
}

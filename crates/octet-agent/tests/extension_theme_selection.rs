#![allow(missing_docs)]

//! Behavioural tests for the API 0.3 host-mediated theme-selection capability.
//!
//! The capability is bounded and fail-closed: unknown namespaces, theme ids, and
//! roles are rejected, a selection can never widen project trust, and two
//! extensions cannot shadow each other's namespace.

use octet_agent::extension_api_v03::{
    capability_spec, method_spec, parse_theme_select_params, resolve_theme_selection,
    theme_role_is_known, theme_trust_is_non_widening, validate_theme_select_params,
    validate_theme_select_result, ContractError, MethodDirection, ThemeSelectParams,
    ThemeSelectResult, MAX_THEME_ID_BYTES, THEME_ROLES, THEME_TRUST_VALUES,
};

fn params() -> ThemeSelectParams {
    ThemeSelectParams {
        namespace: "ext.alpha".to_owned(),
        theme_id: "solarized".to_owned(),
        role: "accent".to_owned(),
        scope: "extension".to_owned(),
    }
}

fn error(result: Result<ThemeSelectResult, ContractError>) -> ContractError {
    match result {
        Ok(value) => panic!("theme selection must fail closed, got {value:?}"),
        Err(error) => error,
    }
}

#[test]
fn valid_selection_resolves_under_the_requesting_namespace() {
    let result = resolve_theme_selection(
        &params(),
        "ext.alpha",
        &[("solarized", "compiled"), ("octet-dark", "user")],
    )
    .expect("valid selection");
    assert_eq!(result.status, "selected");
    assert_eq!(result.theme_id.as_deref(), Some("solarized"));
    assert!(result.reason.is_none());
}

#[test]
fn unknown_namespace_is_rejected() {
    let failure = error(resolve_theme_selection(
        &params(),
        "ext.beta",
        &[("solarized", "compiled")],
    ));
    assert_eq!(failure.code, -32011);
    assert!(failure.message.contains("namespace"));
}

#[test]
fn two_extensions_cannot_shadow_each_other() {
    let catalog = [("solarized", "compiled")];
    let mut beta_params = params();
    beta_params.namespace = "ext.beta".to_owned();
    assert!(resolve_theme_selection(&params(), "ext.alpha", &catalog).is_ok());
    assert!(resolve_theme_selection(&beta_params, "ext.beta", &catalog).is_ok());
    // Alpha can never satisfy a beta-namespaced request.
    let failure = error(resolve_theme_selection(&beta_params, "ext.alpha", &catalog));
    assert_eq!(failure.code, -32011);
}

#[test]
fn unknown_theme_id_is_rejected() {
    let mut candidate = params();
    candidate.theme_id = "does-not-exist".to_owned();
    let failure = error(resolve_theme_selection(&candidate, "ext.alpha", &[]));
    assert_eq!(failure.code, -32602);
    assert!(failure.message.contains("unknown theme id"));
}

#[test]
fn unknown_role_is_rejected() {
    let mut candidate = params();
    candidate.role = "not-a-role".to_owned();
    let failure = match validate_theme_select_params(&candidate) {
        Ok(()) => panic!("unknown role must be rejected"),
        Err(error) => error,
    };
    assert_eq!(failure.code, -32602);
}

#[test]
fn every_known_role_is_accepted() {
    for role in THEME_ROLES {
        let mut candidate = params();
        candidate.role = (*role).to_owned();
        validate_theme_select_params(&candidate).expect("known role");
        assert!(theme_role_is_known(role));
    }
}

#[test]
fn scope_cannot_be_widened() {
    // Only the extension scope is representable, so a request can never widen
    // project trust through this capability.
    for scope in ["project", "global", "workspace"] {
        let mut candidate = params();
        candidate.scope = scope.to_owned();
        let failure = match validate_theme_select_params(&candidate) {
            Ok(()) => panic!("scope {scope} must be rejected"),
            Err(error) => error,
        };
        assert_eq!(failure.code, -32602);
    }
}

#[test]
fn widening_trust_is_rejected() {
    let failure = error(resolve_theme_selection(
        &params(),
        "ext.alpha",
        &[("solarized", "project")],
    ));
    assert_eq!(failure.code, -32011);
    assert!(failure.message.contains("trust"));
    assert!(!theme_trust_is_non_widening("project"));
    assert!(theme_trust_is_non_widening("compiled"));
    assert!(theme_trust_is_non_widening("user"));
    assert_eq!(THEME_TRUST_VALUES, ["compiled", "user"]);
}

#[test]
fn unknown_trust_value_is_rejected() {
    let failure = error(resolve_theme_selection(
        &params(),
        "ext.alpha",
        &[("solarized", "root")],
    ));
    assert_eq!(failure.code, -32011);
}

#[test]
fn overlong_theme_id_is_rejected() {
    let mut candidate = params();
    candidate.theme_id = "t".repeat(MAX_THEME_ID_BYTES + 1);
    let failure = match validate_theme_select_params(&candidate) {
        Ok(()) => panic!("overlong theme id must be rejected"),
        Err(error) => error,
    };
    assert_eq!(failure.code, -32012);
}

#[test]
fn unknown_fields_are_rejected() {
    let value = serde_json::json!({
        "namespace": "ext.alpha",
        "theme_id": "solarized",
        "role": "accent",
        "scope": "extension",
        "persist": true,
    });
    let failure = match parse_theme_select_params(value) {
        Ok(parsed) => panic!("unknown field must be rejected, got {parsed:?}"),
        Err(error) => error,
    };
    assert_eq!(failure.code, -32602);
}

#[test]
fn result_status_is_bounded() {
    let result = ThemeSelectResult {
        status: "escalated".to_owned(),
        theme_id: None,
        reason: None,
    };
    assert_eq!(
        validate_theme_select_result(&result)
            .expect_err("unknown status must be rejected")
            .code,
        -32602
    );
}

#[test]
fn capability_and_method_are_registered_as_optional_but_available() {
    let capability = capability_spec("theme_selection").expect("capability registered");
    assert!(!capability.required_by_default);
    assert!(capability.available);
    let method = method_spec("theme/select").expect("method registered");
    assert_eq!(method.direction, MethodDirection::ExtensionToHost);
    assert_eq!(method.capability, "theme_selection");
    assert!(!method.required_by_default);
    assert!(method.available);
    assert_eq!(method.params, Some("ThemeSelectParams"));
    assert_eq!(method.result, Some("ThemeSelectResult"));
    assert!(!method.notification);
}

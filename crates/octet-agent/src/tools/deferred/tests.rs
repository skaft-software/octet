//! Unit tests for durable suspend/resume and poll permits.
//!
//! Separate from `deferred.rs` so the state machine that owns a suspended
//! invocation is readable without interleaved test bodies.
use super::*;

fn identity() -> ModelIdentity {
    ModelIdentity::new("test-provider", "test-model")
}

fn handle() -> DeferredHandle {
    DeferredHandle::new("test-provider", "test-model", "test-api", "handle-1")
}

fn effect_pending_leaf() -> DeferredSuspended {
    DeferredSuspended {
        operation_id: "op-1".to_owned(),
        source_entry_id: "entry-1".to_owned(),
        identity: identity(),
        response_api: "test-api".to_owned(),
        poll: 3,
        phase: DeferredPhase::EffectPending {
            response_id: "abandoned-response".to_owned(),
            usage_id: "abandoned-usage".to_owned(),
        },
        handle: handle(),
        generation: 7,
    }
}

#[test]
fn handle_debug_redacts_conversion_data() {
    let mut handle = handle();
    handle.data = Some(serde_json::json!({
        "provider_token": "secret-provider-material",
    }));
    let debug = format!("{handle:?}");
    assert!(
        debug.contains("[REDACTED]"),
        "the conversion data slot must be marked redacted: {debug}"
    );
    assert!(
        !debug.contains("secret-provider-material"),
        "provider conversion data must never reach Debug: {debug}"
    );
    // The typed identity stays debuggable: redaction covers `data` only.
    assert!(debug.contains("handle-1"));
}

#[test]
fn a_plain_permit_never_replaces_an_unknown_outcome_poll() {
    let leaf = effect_pending_leaf();
    let mut permit = DeferredPollPermit::one("pass-1", leaf.generation);
    let preparation = prepare_deferred_poll(&leaf, &mut permit, 0, || "fresh".to_owned());
    match preparation {
        DeferredPollPreparation::Refused(refusal) => {
            assert_eq!(
                refusal.kind,
                DeferredPollRefusalKind::UnknownPollOutcome { poll: 3 }
            );
            assert!(refusal.diagnostic.contains("unknown outcome"));
        }
        other => panic!("an unknown outcome must be refused, got {other:?}"),
    }
    assert_eq!(permit.remaining(), 1, "a refusal spends no permit");
    assert!(!permit.is_consumed());
    assert!(matches!(
        prepare_deferred_poll(
            &leaf,
            &mut DeferredPollPermit::none("pass-2", leaf.generation),
            0,
            || "fresh".to_owned()
        ),
        DeferredPollPreparation::Waiting(_)
    ));
}

#[test]
fn an_explicit_replacement_resumes_the_unknown_outcome_under_fresh_ids() {
    let leaf = effect_pending_leaf();
    let mut permit = DeferredPollPermit::one_replacing_unknown("pass-2", leaf.generation);
    let preparation = prepare_deferred_poll(&leaf, &mut permit, 0, || "fresh".to_owned());
    let DeferredPollPreparation::Admitted(intent) = preparation else {
        panic!("an explicit replacement must be admitted, got {preparation:?}");
    };
    assert_eq!(intent.poll, 3, "a poll is not a new request");
    let DeferredPhase::EffectPending {
        response_id,
        usage_id,
    } = &intent.phase
    else {
        panic!("an admitted replacement is effect pending");
    };
    assert_eq!(response_id, "fresh");
    assert_eq!(usage_id, "fresh");
    let replacement = intent
        .discard_unknown_poll
        .expect("the abandoned reservation must be reported");
    assert_eq!(replacement.abandoned_response_id, "abandoned-response");
    assert_eq!(replacement.abandoned_usage_id, "abandoned-usage");
}

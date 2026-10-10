//! A01/A04/A08 using the actual Python SDK and production host byte boundary.
use super::{common, valid, Fixture};
use serde_json::{json, Value};
use std::time::Duration;

pub(super) fn canonical(mut value: Value) -> Value {
    match &mut value {
        Value::Object(fields) => {
            for annotation in ["title", "description", "$schema"] {
                fields.remove(annotation);
            }
            for (key, child) in fields.iter_mut() {
                *child = canonical(child.take());
                if matches!(key.as_str(), "required" | "anyOf") {
                    if let Some(items) = child.as_array_mut() {
                        items.sort_by_key(|item| serde_json::to_string(item).unwrap());
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = canonical(item.take());
            }
        }
        _ => {}
    }
    value
}

async fn fixture() -> Fixture {
    Fixture::start_fixture(
        "values_matrix_fixture.py",
        &[
            "typed_roundtrip",
            "invalid_output",
            "typed_wait",
            "typed_progress",
            "typed_diagnostics",
            "malformed_diagnostics",
            "defaults",
            "hostile",
        ],
    )
    .await
}

#[tokio::test]
async fn a01_shared_schema_and_values() {
    let f = fixture().await;
    let definition = f
        .process
        .tool_definitions()
        .into_iter()
        .find(|tool| tool.name == "typed_roundtrip")
        .unwrap();
    let expected = canonical(common()["cases"][0]["schema"].clone());
    assert_eq!(canonical(definition.parameters), expected);
    assert_eq!(canonical(definition.output_schema.unwrap()), expected);
    for input in common()["cases"][0]["valid"].as_array().unwrap() {
        let output = f.call("typed_roundtrip", input.clone()).await.unwrap();
        let mut expected = input.clone();
        if expected.get("note").is_none() {
            expected["note"] = Value::Null;
        }
        expected["samples"] = expected["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| json!(n.as_f64().unwrap()))
            .collect();
        assert_eq!(output.structured_content, Some(expected));
        assert_eq!(output.content, "Echoed typed record.");
    }
    f.shutdown().await;
}

#[tokio::test]
async fn a04_required_nullable_nonnull_defaults_and_fresh_containers() {
    let f = fixture().await;
    let tool = f
        .process
        .tool_definitions()
        .into_iter()
        .find(|tool| tool.name == "defaults")
        .unwrap();
    assert_eq!(tool.parameters["required"], json!(["required_nullable"]));
    assert_eq!(
        tool.parameters["properties"]["default_nullable"]["default"],
        "fallback"
    );
    assert_eq!(tool.parameters["properties"]["count"]["default"], 7);
    assert_eq!(
        tool.parameters["properties"]["values"]["default"],
        json!([])
    );
    assert_eq!(tool.output_schema, Some(tool.parameters.clone()));
    for input in [json!({}), json!({"required_nullable":null,"count":null})] {
        let before = f.log();
        assert!(f.call("defaults", input).await.is_err());
        assert_eq!(
            f.log(),
            before,
            "invalid optional input entered domain handler"
        );
    }
    for input in [
        json!({"required_nullable":null}),
        json!({"required_nullable":null}),
        json!({"required_nullable":"required", "default_nullable":null}),
        json!({"required_nullable":"required", "default_nullable":"present","count":9,"values":[1]}),
    ] {
        let mut expected = input.clone();
        if expected.get("default_nullable").is_none() {
            expected["default_nullable"] = json!("fallback");
        }
        if expected.get("count").is_none() {
            expected["count"] = json!(7);
        }
        if expected.get("values").is_none() {
            expected["values"] = json!([]);
        }
        let count = expected["count"].clone();
        expected["values"].as_array_mut().unwrap().push(count);
        let output = f.call("defaults", input).await.unwrap();
        assert_eq!(output.structured_content, Some(expected));
        assert_eq!(output.content, "Defaults decoded.");
    }
    f.shutdown().await;
}

async fn fatal_frame(mode: &str, message: &str) {
    let f = fixture().await;
    let generation = f.process.health_snapshot().generation;
    assert!(!f.call("typed_roundtrip", valid()).await.unwrap().is_error);
    let error = f.call("hostile", json!({"mode":mode})).await.unwrap_err();
    let refusal = error.to_string();
    assert!(refusal.len() < 4096, "unbounded transport refusal");
    assert!(refusal.contains(message), "unexpected refusal: {refusal}");
    f.barrier(&format!("fault_{mode}")).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.process.is_running() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded generation termination");
    f.process.shutdown().await;
    assert!(!f.process.is_running());
    let before = f.log();
    assert!(f.call("typed_roundtrip", valid()).await.is_err());
    assert_eq!(
        f.log(),
        before,
        "closed generation dispatched another handler"
    );
    assert_eq!(f.process.health_snapshot().generation, generation);
    assert_eq!(
        before
            .iter()
            .filter(|row| row["event"] == "hostile")
            .count(),
        1
    );
    assert!(before.iter().all(|row| row["pid"] == before[0]["pid"]));
    println!(
        "hostile transport {mode}: {refusal}; child log: {}",
        json!(before)
    );
}

#[tokio::test]
async fn a08_invalid_frame_terminates_generation() {
    fatal_frame("invalid", "invalid JSON").await;
}
#[tokio::test]
async fn a08_oversize_frame_terminates_generation() {
    fatal_frame("oversize", "exceeded").await;
}
#[tokio::test]
async fn a08_eof_settles_pending_call() {
    fatal_frame("eof", "stdout closed").await;
}

#[tokio::test]
async fn a08_duplicate_terminal_does_not_poison_next_call_and_shutdown() {
    let f = fixture().await;
    let generation = f.process.health_snapshot().generation;
    let output = f
        .call("hostile", json!({"mode":"duplicate"}))
        .await
        .unwrap();
    assert_eq!(output.structured_content, Some(json!(42)));
    // FIFO stdout makes the duplicate precede this independent response. It
    // cannot settle/replay another call or replace its typed structured value.
    assert_eq!(
        f.call("typed_roundtrip", valid()).await.unwrap().content,
        "Echoed typed record."
    );
    assert_eq!(
        f.log()
            .iter()
            .filter(|row| row["event"] == "hostile")
            .count(),
        1
    );
    assert_eq!(
        f.log()
            .iter()
            .filter(|row| row["event"] == "typed_roundtrip")
            .count(),
        1
    );
    assert_eq!(f.process.health_snapshot().generation, generation);
    f.shutdown().await;
}

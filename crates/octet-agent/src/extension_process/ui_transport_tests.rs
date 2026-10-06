//! Native autocomplete edit wire contract; real local peers, no model calls.
use super::*;
use serde_json::{json, Value};

fn request(text: &str, cursor: usize) -> ExtensionAutocompleteRequest {
    ExtensionAutocompleteRequest {
        text: text.into(),
        cursor,
        revision: 1,
    }
}

fn response(prefix: &str, value: &str) -> Value {
    json!({"prefix":prefix,"items":[{"value":value,"label":"choice"}]})
}

#[test]
fn autocomplete_edit_presence_numbers_controls_and_snapshot_ranges() {
    let legacy = response("é", "dir");
    let check = |wire: Value, request: &ExtensionAutocompleteRequest, negotiated| {
        serde_json::from_value::<ExtensionAutocompleteResponse>(wire)
            .map_err(|error| error.to_string())
            .and_then(|response| response.validate_for_request(request, negotiated))
    };
    let original = request("é\"", 2);
    assert!(check(legacy.clone(), &original, false).is_ok());
    for field in ["replace_after_bytes", "cursor_offset_bytes"] {
        for value in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(true),
            json!(4294967296u64),
        ] {
            let mut wire = legacy.clone();
            wire["items"][0][field] = value;
            assert!(check(wire, &original, true).is_err(), "{field}");
        }
        let mut wire = legacy.clone();
        wire["items"][0][field] = json!(0);
        assert!(check(wire.clone(), &original, false).is_err());
        assert!(check(wire, &original, true).is_ok());
    }
    for (prefix, value) in [("é\n\t\r", "文\n\t\r"), ("", "x")] {
        let req = request(prefix, prefix.len());
        assert!(check(response(prefix, value), &req, true).is_ok());
        if !prefix.is_empty() {
            assert!(check(response(prefix, value), &req, false).is_err());
        }
    }
    for field in ["label", "description"] {
        for control in ["\n", "\t", "\r", "\u{1b}", "\0", "\u{85}"] {
            let mut wire = legacy.clone();
            wire["items"][0][field] = json!(control);
            assert!(check(wire, &original, true).is_err());
        }
    }
    for (field, value) in [
        ("replace_after_bytes", json!(262145)),
        ("replace_after_bytes", json!(2)),
        ("cursor_offset_bytes", json!(4)),
        ("unknown", json!(0)),
    ] {
        let mut wire = legacy.clone();
        wire["items"][0][field] = value;
        assert!(check(wire, &original, true).is_err(), "{field}");
    }
    let mut wire = response("", "é");
    wire["items"][0]["cursor_offset_bytes"] = json!(1);
    assert!(check(wire, &request("", 0), true).is_err());
    let mut wire = response("", "x");
    wire["items"][0]["replace_after_bytes"] = json!(1);
    assert!(check(wire, &request("é", 0), true).is_err());
    assert!(check(response("wrong", "x"), &original, true).is_err());
    assert!(check(legacy.clone(), &request("é", 1), true).is_err());
    assert!(check(response("", &"é".repeat(513)), &original, true).is_err());
    let full = "x".repeat(262144);
    assert!(check(response("", "x"), &request(&full, full.len()), true).is_err());
    let mut wire = response("", "x");
    wire["items"][0]["replace_after_bytes"] = json!(262144);
    assert!(check(wire, &request(&full, 0), true).is_ok());
    let mut wire = legacy;
    wire["unknown"] = json!(true);
    assert!(check(wire, &original, true).is_err());
}

#[cfg(unix)]
const PEER: &str = r#"#!/usr/bin/env python3
import json, os, sys
config_path = os.path.join(os.environ['OCTET_WORKSPACE'], 'peer.json')
for line in sys.stdin:
    request = json.loads(line)
    config = json.load(open(config_path))
    with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'peer.jsonl'), 'a') as log:
        log.write(json.dumps({'pid': os.getpid(), 'request': request}) + '\n')
    method = request.get('method')
    if method == 'initialize':
        offered = request['params']['protocol']['optional_features']
        assert ('autocomplete_edit_v1' in offered) == (config['api'] == '0.4')
        result = {'api_version': config['api'], 'tools': [], 'protocol': {
            'version': config['api'], 'features': config['features'],
            'limits': {'max_concurrent_requests': 1}}}
    elif method == 'ui/autocomplete/complete':
        result = config['response']
    elif method == 'shutdown':
        print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{}}), flush=True)
        break
    else:
        continue
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#;

#[cfg(unix)]
async fn peer(
    root: &Path,
    api: &str,
    features: &[&str],
    wire: Value,
) -> Result<ExtensionProcess, ExtensionRuntimeError> {
    tests::write_executable_script(&root.join("peer.py"), PEER);
    std::fs::write(
        root.join("peer.json"),
        serde_json::to_vec(&json!({
            "api":api,"features":features,"response":wire
        }))
        .unwrap(),
    )
    .unwrap();
    let manifest = ExtensionManifest::parse(&format!("name = \"autocomplete-edit\"\nversion = \"1.0.0\"\napi_version = \"{api}\"\n[entrypoint]\ncommand = \"peer.py\"\n")).unwrap();
    ExtensionProcess::start(
        tests::trusted_descriptor(root, manifest),
        ExtensionRuntimeConfig::new(root),
    )
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn autocomplete_edit_real_peer_negotiation_requires_api04_and_autocomplete() {
    for (api, features) in [
        (
            "0.2",
            vec![
                "request_cancellation",
                "content_parts",
                "autocomplete",
                "autocomplete_edit_v1",
            ],
        ),
        (
            "0.4",
            vec![
                "request_cancellation",
                "content_parts",
                "autocomplete_edit_v1",
            ],
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        assert!(peer(root.path(), api, &features, response("", "x"))
            .await
            .is_err());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn autocomplete_edit_real_peer_legacy_presence_and_exact_edit_round_trip() {
    for (api, edits) in [("0.2", false), ("0.4", false), ("0.4", true)] {
        let root = tempfile::tempdir().unwrap();
        let mut features = vec!["request_cancellation", "content_parts", "autocomplete"];
        if edits {
            features.push("autocomplete_edit_v1");
        }
        let process = peer(root.path(), api, &features, response("é", "dir"))
            .await
            .unwrap();
        let req = request("é\"", 2);
        let legacy = process.request_autocomplete(req.clone()).await.unwrap();
        assert_eq!(legacy.items[0].replace_after_bytes, None);
        assert_eq!(legacy.items[0].cursor_offset_bytes, None);
        for invalid in [false, true] {
            let mut wire = response("é", "文/\"");
            wire["items"][0]["replace_after_bytes"] = json!(1);
            wire["items"][0]["cursor_offset_bytes"] = if invalid { Value::Null } else { json!(4) };
            std::fs::write(
                root.path().join("peer.json"),
                serde_json::to_vec(&json!({"api":api,"features":features,"response":wire}))
                    .unwrap(),
            )
            .unwrap();
            let result = process.request_autocomplete(req.clone()).await;
            if edits && !invalid {
                let result = result.unwrap();
                assert_eq!(result.items[0].replace_after_bytes, Some(1));
                assert_eq!(result.items[0].cursor_offset_bytes, Some(4));
            } else {
                assert!(result.is_err());
            }
        }
        assert!(process.shutdown().await);
        eprintln!(
            "autocomplete peer evidence: {}",
            std::fs::read_to_string(root.path().join("peer.jsonl")).unwrap()
        );
    }
}

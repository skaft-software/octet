//! E host-wire cases: real independent peers, not SDK author-API parity.
use super::*;
use crate::{BlobRef, BulkLimits, BulkStorage};
#[cfg(unix)]
mod faults;
mod presentation;

const PAYLOAD_BYTES: u64 = 47_104;
fn blob_schema() -> Value {
    json!({"type":"object","properties":{"$blob":{"type":"string"},"bytes":{"type":"integer","minimum":0,"maximum":9007199254740991_u64},"digest":{"type":"object","properties":{"algorithm":{"type":"string","const":"sha256"},"value":{"type":"string"}},"required":["algorithm","value"],"additionalProperties":false},"media_type":{"type":"string"}},"required":["$blob","bytes","digest","media_type"],"additionalProperties":false})
}
fn tools(resources: bool) -> Value {
    let mut controls = json!({"capacity":{"type":"integer"},"length_delta":{"type":"integer"},"profile":{"type":"string"}});
    for key in [
        "mixed",
        "wrong_digest",
        "oversize",
        "rewrite",
        "diagnostic",
        "attachment_only",
        "bad_blob",
        "bad_resource",
        "invalid",
        "invalid_diagnostic",
        "locator_leak",
        "error",
        "block",
    ] {
        controls[key] = json!({"type":"boolean"});
    }
    let mut output = json!({"blob":blob_schema(),"leak":{"type":"string"}});
    if resources {
        output["resource"] = ref_schema();
    }
    let mut create = json!({"name":"bulk_create","description":"Create an immutable test blob","parameters":{"type":"object","properties":controls,"additionalProperties":false},"output_schema":{"type":"object","properties":output,"additionalProperties":false}});
    if resources {
        create["operation"] = json!({"id":"demo.bulk_create","resource_inputs":[],"resource_outputs":[{"path":"/resource","type":"demo.Circuit"}]});
    }
    let mut tools = if resources {
        catalog().as_array().unwrap().clone()
    } else {
        vec![catalog()[2].clone()]
    };
    tools.extend([create,
        json!({"name":"bulk_read","description":"Read and verify native bytes","parameters":{"type":"object","properties":{"blob":blob_schema(),"keep":{"type":"boolean"},"profile":{"type":"string"}},"required":["blob"],"additionalProperties":false},"output_schema":{"type":"object","properties":{"bytes":{"type":"integer"},"verified":{"type":"boolean"}},"required":["bytes","verified"],"additionalProperties":false}}),
        json!({"name":"bulk_release","description":"Close an opaque transfer grant","parameters":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false},"output_schema":{"type":"object","properties":{"released":{"type":"boolean"}},"required":["released"],"additionalProperties":false}})
    ]);
    json!(tools)
}
fn storage() -> BulkStorage {
    BulkStorage::with_limits(BulkLimits {
        object_bytes: PAYLOAD_BYTES * 2,
        owner_bytes: PAYLOAD_BYTES * 8,
        write_tickets_per_generation: 2,
        read_leases_per_generation: 2,
        blobs_per_owner: 8,
    })
    .unwrap()
}
async fn fixture(rust: bool, controlled: bool, storage: &BulkStorage, resources: bool) -> Fixture {
    Fixture::start_custom(
        rust,
        controlled,
        4,
        false,
        Some(storage.clone()),
        tools(resources),
        resources,
    )
    .await
}
fn blob(output: &ToolCallOutput) -> BlobRef {
    serde_json::from_value(output.structured_content.as_ref().unwrap()["blob"].clone()).unwrap()
}
async fn verify(fixture: &Fixture, owner: &str, blob: &BlobRef) {
    let output = fixture
        .call(owner, "bulk_read", json!({"blob":blob}))
        .await
        .unwrap();
    assert!(!output.is_error);
    assert_eq!(
        output.structured_content,
        Some(json!({"bytes":PAYLOAD_BYTES,"verified":true}))
    );
}
async fn unavailable_blob(fixture: &Fixture, owner: &str, blob: &BlobRef) {
    let count = fixture.calls();
    assert!(fixture
        .call(owner, "bulk_read", json!({"blob":blob}))
        .await
        .unwrap_err()
        .to_string()
        .contains("blob_unavailable"));
    assert_eq!(
        fixture.calls(),
        count,
        "refusal must precede target invocation"
    );
}

#[tokio::test]
async fn b01_lifecycle_b08_release_b12_immutable_snapshot_without_resource_feature() {
    for (rust, controlled) in VARIANTS {
        let storage = storage();
        let mut f = fixture(rust, controlled, &storage, false).await;
        assert!(!f
            .process
            .negotiated_protocol()
            .supports(EXTENSION_FEATURE_RESOURCE_REFS_V1));
        let reference = blob(
            &f.call(
                "A",
                "bulk_create",
                json!({"rewrite":true,"diagnostic":true}),
            )
            .await
            .unwrap(),
        );
        verify(&f, "A", &reference).await;
        let output = f
            .call("A", "bulk_read", json!({"blob":reference,"keep":true}))
            .await
            .unwrap();
        assert!(!output.is_error);
        // First lease event belongs to verify(), which closed it already.
        let first = f.event("lease").await;
        let current = f.event("lease").await;
        assert!(
            f.call("A", "bulk_release", json!({"id":first["lease"]}))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            !f.call("A", "bulk_release", json!({"id":current["lease"]}))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            f.call("A", "bulk_release", json!({"id":current["lease"]}))
                .await
                .unwrap()
                .is_error
        );
        verify(&f, "A", &reference).await;
        f.process.shutdown().await;
    }
}

#[tokio::test]
async fn b02_wrong_size_b03_wrong_digest_b04_oversize_and_profile_refusal() {
    for (rust, controlled) in VARIANTS {
        let storage = storage();
        let mut f = fixture(rust, controlled, &storage, false).await;
        for (args, code) in [
            (json!({"length_delta":-1}), "size_mismatch"),
            (
                json!({"length_delta":1,"capacity":PAYLOAD_BYTES+1}),
                "size_mismatch",
            ),
            (json!({"wrong_digest":true}), "integrity_mismatch"),
            (json!({"oversize":true}), "quota_exceeded"),
            (json!({"capacity":PAYLOAD_BYTES*2+1}), "quota_exceeded"),
            (json!({"profile":"remote-file.v2"}), "unsupported_feature"),
        ] {
            let output = f.call("A", "bulk_create", args).await.unwrap();
            assert!(output.is_error);
            assert!(output.structured_content.is_none());
            assert_eq!(f.event("bulk_error").await["code"], code);
        }
        let reference = blob(&f.call("A", "bulk_create", json!({})).await.unwrap());
        verify(&f, "A", &reference).await;
        f.process.shutdown().await;
    }
}

#[tokio::test]
async fn b09_foreign_grant_b16_identity_not_digest_and_shared_same_session() {
    for (rust, controlled) in VARIANTS {
        let storage = storage();
        let a = fixture(rust, controlled, &storage, false).await;
        let b = fixture(rust, controlled, &storage, false).await;
        let reference = blob(&a.call("A", "bulk_create", json!({})).await.unwrap());
        unavailable_blob(&b, "B", &reference).await;
        verify(&b, "A", &reference).await;
        let second = blob(&b.call("B", "bulk_create", json!({})).await.unwrap());
        assert_eq!(reference.digest, second.digest);
        assert_ne!(reference.id, second.id);
        unavailable_blob(&a, "A", &second).await;
        let mut wrong = reference.clone();
        wrong.bytes += 1;
        unavailable_blob(&a, "A", &wrong).await;
        a.process.shutdown().await;
        b.process.shutdown().await;
    }
}

#[tokio::test]
async fn b10_extension_restart_and_owner_roundtrip_retains_blobs_not_native_refs_or_leases() {
    for (rust, controlled) in VARIANTS {
        let storage = storage();
        let mut f = fixture(rust, controlled, &storage, true).await;
        let output = f
            .call("A", "bulk_create", json!({"mixed":true}))
            .await
            .unwrap();
        let native = reference(&output);
        let bytes = blob(&output);
        f.call("A", "bulk_read", json!({"blob":bytes,"keep":true}))
            .await
            .unwrap();
        let lease = f.event("lease").await;
        f.process.retire_resource_owner("A");
        unavailable(f.call("A", "use", json!({"resource":native})).await);
        assert!(
            f.call("A", "bulk_release", json!({"id":lease["lease"]}))
                .await
                .unwrap()
                .is_error
        );
        verify(&f, "A", &bytes).await;
        f.process.reload().await.unwrap();
        unavailable(f.call("A", "use", json!({"resource":native})).await);
        verify(&f, "A", &bytes).await;
        storage.retire_session("A");
        unavailable_blob(&f, "A", &bytes).await;
        f.process.shutdown().await;
    }
}

#[tokio::test]
async fn b17_mixed_output_atomicity_and_diagnostics_do_not_export_provisionals() {
    for (rust, controlled) in VARIANTS {
        for change in [
            "bad_blob",
            "bad_resource",
            "invalid",
            "invalid_diagnostic",
            "attachment_only",
            "locator_leak",
            "error",
        ] {
            let storage = storage();
            let mut f = fixture(rust, controlled, &storage, true).await;
            let mut args = json!({"mixed":true});
            args[change] = json!(true);
            if change == "error" {
                args["diagnostic"] = json!(true);
            }
            assert!(f.call("A", "bulk_create", args).await.is_err(), "{change}");
            let ready = f.event("output_ready").await;
            let native: ResourceRef =
                serde_json::from_value(ready["resources"][0].clone()).unwrap();
            let bytes: BlobRef = serde_json::from_value(ready["blobs"][0].clone()).unwrap();
            unavailable(f.call("A", "use", json!({"resource":native})).await);
            unavailable_blob(&f, "A", &bytes).await;
            f.event("disposed").await;
            f.process.shutdown().await;
        }
    }
}

#[tokio::test]
async fn b06_cancel_copy_and_b05_abandon_before_publication() {
    for (rust, controlled) in VARIANTS {
        let storage = storage();
        let f = fixture(rust, controlled, &storage, false).await;
        let barrier = Arc::new(ReferenceTestBarrier::default());
        lock_std_mutex(&read_std_lock(&f.process.inner.connection).resources).before_bulk_copy =
            Some(barrier.clone());
        let cancellation = CancellationToken::default();
        let running = f.blocked("bulk_create", json!({}), cancellation.clone());
        barrier.entered.notified().await;
        let transfer = storage.lock().transfer_directory().to_owned();
        assert!(std::fs::read_dir(&transfer).unwrap().any(|entry| entry
            .unwrap()
            .metadata()
            .unwrap()
            .len()
            == PAYLOAD_BYTES));
        if controlled {
            cancellation.cancel();
            assert!(running.await.unwrap().is_err());
        } else {
            running.abort();
            assert!(running.await.unwrap_err().is_cancelled());
        }
        barrier.proceed.notify_one();
        until(|| std::fs::read_dir(&transfer).unwrap().next().is_none()).await;
        let reference = blob(&f.call("A", "bulk_create", json!({})).await.unwrap());
        verify(&f, "A", &reference).await;
        f.process.shutdown().await;
    }
}

#[tokio::test]
async fn b06_r09_r10_host_admission_barrier_forces_both_joint_dispositions() {
    for (rust, controlled) in VARIANTS {
        for cancel_first in [true, false] {
            let storage = storage();
            let mut f = fixture(rust, controlled, &storage, true).await;
            let barrier = Arc::new(ReferenceTestBarrier::default());
            lock_std_mutex(&read_std_lock(&f.process.inner.connection).resources)
                .before_result_admission = Some(barrier.clone());
            let cancellation = CancellationToken::default();
            let running = f.blocked("bulk_create", json!({"mixed":true}), cancellation.clone());
            barrier.entered.notified().await;
            let ready = f.event("output_ready").await;
            let native: ResourceRef =
                serde_json::from_value(ready["resources"][0].clone()).unwrap();
            let bytes: BlobRef = serde_json::from_value(ready["blobs"][0].clone()).unwrap();
            assert!(f.process.lookup_resource("A", &native).is_err());
            unavailable_blob(&f, "A", &bytes).await;
            if cancel_first {
                if controlled {
                    cancellation.cancel();
                    assert!(running.await.unwrap().is_err());
                } else {
                    running.abort();
                    assert!(running.await.unwrap_err().is_cancelled());
                }
                barrier.proceed.notify_one();
                unavailable(f.call("A", "use", json!({"resource":native})).await);
                unavailable_blob(&f, "A", &bytes).await;
            } else {
                barrier.proceed.notify_one();
                assert!(!running.await.unwrap().unwrap().is_error);
                cancellation.cancel();
                f.call("A", "use", json!({"resource":native}))
                    .await
                    .unwrap();
                verify(&f, "A", &bytes).await;
            }
            f.process.shutdown().await;
        }
    }
}

#[tokio::test]
async fn b14_retained_byte_and_read_lease_quotas_recover() {
    for (rust, controlled) in VARIANTS {
        let storage = BulkStorage::with_limits(BulkLimits {
            object_bytes: PAYLOAD_BYTES,
            owner_bytes: PAYLOAD_BYTES,
            write_tickets_per_generation: 1,
            read_leases_per_generation: 1,
            blobs_per_owner: 1,
        })
        .unwrap();
        let mut f = fixture(rust, controlled, &storage, false).await;
        let reference = blob(&f.call("A", "bulk_create", json!({})).await.unwrap());
        assert!(
            f.call("A", "bulk_create", json!({}))
                .await
                .unwrap()
                .is_error
        );
        assert_eq!(f.event("bulk_error").await["code"], "quota_exceeded");
        f.call("A", "bulk_read", json!({"blob":reference,"keep":true}))
            .await
            .unwrap();
        let lease = f.event("lease").await;
        assert!(
            f.call("A", "bulk_read", json!({"blob":reference}))
                .await
                .unwrap()
                .is_error
        );
        assert_eq!(f.event("bulk_error").await["code"], "quota_exceeded");
        f.call("A", "bulk_release", json!({"id":lease["lease"]}))
            .await
            .unwrap();
        verify(&f, "A", &reference).await;
        storage.release_blob("A", &reference).unwrap();
        unavailable_blob(&f, "A", &reference).await;
        let replacement = blob(&f.call("A", "bulk_create", json!({})).await.unwrap());
        assert_ne!(reference.id, replacement.id);
        f.process.shutdown().await;
    }
}

#[tokio::test]
async fn b13_real_symlink_substitution_and_path_like_grants_are_refused() {
    for (rust, controlled) in VARIANTS {
        let storage = storage();
        let mut f = fixture(rust, controlled, &storage, false).await;
        for id in ["../outside", "/tmp/outside", "unissued-grant"] {
            assert!(
                f.call("A", "bulk_release", json!({"id":id}))
                    .await
                    .unwrap()
                    .is_error
            );
            assert_eq!(f.event("bulk_error").await["code"], "blob_unavailable");
        }
        let barrier = Arc::new(ReferenceTestBarrier::default());
        lock_std_mutex(&read_std_lock(&f.process.inner.connection).resources).before_bulk_copy =
            Some(barrier.clone());
        let running = f.blocked("bulk_create", json!({}), CancellationToken::default());
        barrier.entered.notified().await;
        let transfer = storage.lock().transfer_directory().to_owned();
        let scratch = std::fs::read_dir(&transfer)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let outside = f.temp.path().join("outside-sentinel");
        std::fs::write(&outside, b"must not be read or changed").unwrap();
        std::fs::remove_file(&scratch).unwrap();
        std::os::unix::fs::symlink(&outside, &scratch).unwrap();
        barrier.proceed.notify_one();
        assert!(running.await.unwrap().unwrap().is_error);
        assert_eq!(f.event("bulk_error").await["code"], "storage_unavailable");
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"must not be read or changed"
        );
        std::fs::remove_file(&scratch).unwrap();
        let reference = blob(&f.call("A", "bulk_create", json!({})).await.unwrap());
        verify(&f, "A", &reference).await;
        f.process.shutdown().await;
    }
}

#[test]
fn bulk_feature_is_configured_api04_only() {
    assert!(ExtensionRuntimeConfig::new(".").bulk_store.is_none());
    for version in ["0.2", "0.4"] {
        let manifest = ExtensionManifest::parse(&format!("name='bulk-negotiation'\nversion='1.0.0'\napi_version='{version}'\n[entrypoint]\ncommand='fixture'\n")).unwrap();
        for configured in [false, true] {
            let response: InitializeResponse = serde_json::from_value(json!({"api_version":version,"tools":[],"protocol":{"version":version,"features":["request_cancellation","content_parts","bulk_objects_v1"],"limits":{"max_concurrent_requests":4}}})).unwrap();
            assert_eq!(
                negotiate_contributions_with_host_services(
                    &manifest,
                    response,
                    4,
                    OfferedHostServices {
                        bulk_objects: configured,
                        ..OfferedHostServices::default()
                    }
                )
                .is_ok(),
                configured && version == "0.4"
            );
        }
    }
}

#[test]
fn bulk_closed_schema_and_transport_namespace_refusal() {
    validate_bulk_schema(&blob_schema()).unwrap();
    for key in ["additionalProperties", "required"] {
        let mut invalid = blob_schema();
        invalid.as_object_mut().unwrap().remove(key);
        assert!(validate_bulk_schema(&invalid).is_err());
    }
    let mut invalid = blob_schema();
    invalid["properties"]["digest"]["properties"]["algorithm"]["enum"] = json!(["md5"]);
    assert!(validate_bulk_schema(&invalid).is_err());
    let path = Path::new("/private/test/bulk-transfer");
    assert!(validate_no_bulk_locators(
        &json!({"leak":format!("octet-transfer-{}", "a".repeat(48))}),
        path
    )
    .is_err());
    assert!(validate_no_bulk_locators(&json!({"digest":"a".repeat(64)}), path).is_ok());
}

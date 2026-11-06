use super::super::tests::{
    insert_test_parent, protocol_read_state_for_test, test_resource_owner, wave1_line,
};
use super::*;

const MCP_SELECTOR: &str = "-builtin:mcp";

fn state() -> (
    tempfile::TempDir,
    ProtocolReadState,
    mpsc::Receiver<WriterFrame>,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let agent_dir = root.join("agent");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("settings.json"),
        br#"{"extensions":["keep"],"theme":"dark","unrelated":{"enabled":true}}"#,
    )
    .unwrap();
    let (events, _) = broadcast::channel(16);
    let (mut state, frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    state.pi_settings = Some(Arc::new(Service::new(PiSettingsConfig {
        workspace,
        agent_dir,
        project_trusted: false,
    })));
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_4.into();
        protocol.features.insert(FEATURE.into());
    }
    insert_test_parent(&state, 7, Some(test_resource_owner("session")));
    state
        .pi_settings
        .as_ref()
        .unwrap()
        .remember_parent(
            7,
            &test_resource_owner("session"),
            &lock_std_mutex(&state.issued_resource_owners),
        )
        .unwrap();
    (directory, state, frames)
}

fn params(state: &ProtocolReadState, update: bool) -> Value {
    let config = &state.pi_settings.as_ref().unwrap().config;
    let mut params = json!({
        "parent_request_id":7,"resource_owner":test_resource_owner("session"),
        "cwd":config.workspace,"agent_dir":config.agent_dir,"project_trusted":false,
    });
    if update {
        params["scope"] = json!("global");
        params["extensions"] = json!(["keep", MCP_SELECTOR]);
    }
    params
}

fn response(frames: &mut mpsc::Receiver<WriterFrame>) -> Value {
    serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap()
}

fn settings_path(state: &ProtocolReadState) -> PathBuf {
    state
        .pi_settings
        .as_ref()
        .unwrap()
        .config
        .agent_dir
        .join("settings.json")
}

#[test]
fn pi_settings_native_read_and_additive_update_persist_and_preserve_disk_fields() {
    let (_directory, state, mut frames) = state();
    let read = params(&state, false);
    handle_protocol_line(&wave1_line(100, "pi/settings/read", read), &state).unwrap();
    let result = response(&mut frames);
    assert_eq!(result["result"]["global"]["theme"], "dark");
    assert_eq!(result["result"]["project"], json!({}));
    assert_eq!(result["result"]["errors"], json!([]));
    assert_eq!(result["result"]["project_trusted"], false);
    let path = settings_path(&state);
    let mut disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    disk["concurrent"] = json!("preserved");
    std::fs::write(&path, serde_json::to_vec(&disk).unwrap()).unwrap();
    for id in [101, 102] {
        handle_protocol_line(
            &wave1_line(id, "pi/settings/update", params(&state, true)),
            &state,
        )
        .unwrap();
        assert_eq!(response(&mut frames)["result"], json!({}));
    }
    let disk: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(disk["extensions"], json!(["keep", MCP_SELECTOR]));
    assert_eq!(disk["concurrent"], "preserved");
    assert_eq!(disk["theme"], "dark");
    assert_eq!(disk["unrelated"], json!({"enabled":true}));
}

#[test]
fn pi_settings_native_generic_builtin_disable_and_exact_noop_are_idempotent() {
    let (_directory, state, mut frames) = state();
    let path = settings_path(&state);
    let before = std::fs::read(&path).unwrap();
    let mut request = params(&state, true);
    request["extensions"] = json!(["keep"]);
    handle_protocol_line(
        &wave1_line(100, "pi/settings/update", request.clone()),
        &state,
    )
    .unwrap();
    assert_eq!(response(&mut frames)["result"], json!({}));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    request["extensions"] = json!(["keep", "-builtin:sample-extension_2"]);
    handle_protocol_line(
        &wave1_line(101, "pi/settings/update", request.clone()),
        &state,
    )
    .unwrap();
    assert_eq!(response(&mut frames)["result"], json!({}));
    let committed = std::fs::read(&path).unwrap();
    let disk: Value = serde_json::from_slice(&committed).unwrap();
    assert_eq!(disk["extensions"], request["extensions"]);
    assert_eq!(disk["unrelated"], json!({"enabled":true}));
    handle_protocol_line(&wave1_line(102, "pi/settings/update", request), &state).unwrap();
    assert_eq!(response(&mut frames)["result"], json!({}));
    assert_eq!(std::fs::read(&path).unwrap(), committed);
}

#[test]
fn pi_settings_native_rejects_deletion_reorder_duplicates_and_unsafe_disable_selectors() {
    let (_directory, state, mut frames) = state();
    let path = settings_path(&state);
    let before = br#"{"extensions":["keep","second"],"unrelated":true}"#;
    std::fs::write(&path, before).unwrap();
    let mut cases = vec![
        json!([MCP_SELECTOR]),
        json!(["keep", MCP_SELECTOR]),
        json!(["second", "keep", MCP_SELECTOR]),
        json!(["keep", "second", "second", MCP_SELECTOR]),
        json!(["keep", "second", "second"]),
        json!(["keep", "second", MCP_SELECTOR, "-builtin:other"]),
    ];
    for selector in [
        "builtin:mcp",
        "+builtin:mcp",
        "!builtin:mcp",
        "-file.ts",
        "-builtin:",
        "-builtin:*",
        "-builtin:../mcp",
        "-builtin:mcp/other",
        "-builtin:mcp\\other",
        "-builtin:mcp.other",
        "-builtin:Upper",
        "-builtin:mcp\n",
        "-builtin:é",
    ] {
        cases.push(json!(["keep", "second", selector]));
    }
    cases.push(json!([
        "keep",
        "second",
        format!("-builtin:{}", "a".repeat(65))
    ]));
    for (index, extensions) in cases.into_iter().enumerate() {
        let mut request = params(&state, true);
        request["extensions"] = extensions.clone();
        handle_protocol_line(
            &wave1_line(100 + index as u64, "pi/settings/update", request),
            &state,
        )
        .unwrap();
        let refused = response(&mut frames);
        assert_eq!(refused["error"]["code"], -32602, "{extensions}: {refused}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn pi_settings_native_stale_selector_snapshot_refuses_and_preserves_concurrent_additions() {
    let (_directory, state, mut frames) = state();
    handle_protocol_line(
        &wave1_line(100, "pi/settings/read", params(&state, false)),
        &state,
    )
    .unwrap();
    let mut extensions = response(&mut frames)["result"]["global"]["extensions"]
        .as_array()
        .unwrap()
        .clone();
    extensions.push(json!(MCP_SELECTOR));
    let path = settings_path(&state);
    let concurrent = br#"{"extensions":["keep","concurrent-selector"],"concurrent":"preserved"}"#;
    std::fs::write(&path, concurrent).unwrap();
    let mut request = params(&state, true);
    request["extensions"] = json!(extensions);
    handle_protocol_line(
        &wave1_line(101, "pi/settings/update", request.clone()),
        &state,
    )
    .unwrap();
    assert_eq!(response(&mut frames)["error"]["code"], -32602);
    assert_eq!(std::fs::read(&path).unwrap(), concurrent);
    request["extensions"] = json!(["keep", "concurrent-selector", MCP_SELECTOR]);
    handle_protocol_line(&wave1_line(102, "pi/settings/update", request), &state).unwrap();
    assert_eq!(response(&mut frames)["result"], json!({}));
    let disk: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        disk["extensions"],
        json!(["keep", "concurrent-selector", MCP_SELECTOR])
    );
    assert_eq!(disk["concurrent"], "preserved");
}

#[test]
fn pi_settings_native_retained_owner_is_admitted_but_cancelled_and_retired_owners_are_not() {
    let (_directory, state, mut frames) = state();
    lock_std_mutex(&state.pending).remove(&7);
    handle_protocol_line(
        &wave1_line(100, "pi/settings/update", params(&state, true)),
        &state,
    )
    .unwrap();
    assert_eq!(response(&mut frames)["result"], json!({}));
    let before = std::fs::read(settings_path(&state)).unwrap();
    lock_std_mutex(&state.tombstones).insert(7, Duration::from_secs(60));
    handle_protocol_line(
        &wave1_line(101, "pi/settings/update", params(&state, true)),
        &state,
    )
    .unwrap();
    assert!(response(&mut frames).get("error").is_some());
    lock_std_mutex(&state.tombstones).remove(7);
    lock_std_mutex(&state.issued_resource_owners).clear();
    handle_protocol_line(
        &wave1_line(102, "pi/settings/update", params(&state, true)),
        &state,
    )
    .unwrap();
    assert!(response(&mut frames).get("error").is_some());
    assert_eq!(std::fs::read(settings_path(&state)).unwrap(), before);
}

#[test]
fn pi_settings_native_rejects_paths_trust_foreign_owner_and_ungranted_mutations_without_writes() {
    let (_directory, state, mut frames) = state();
    let before = std::fs::read(settings_path(&state)).unwrap();
    let base = params(&state, true);
    let mut cases = Vec::new();
    for (field, value) in [
        ("parent_request_id", json!(999)),
        ("agent_dir", json!("/foreign")),
        ("cwd", json!("/foreign")),
        ("project_trusted", json!(true)),
        ("scope", json!("project")),
        ("extensions", json!([])),
        ("extensions", json!(["new", MCP_SELECTOR])),
        ("unknown", json!(true)),
    ] {
        let mut invalid = base.clone();
        invalid[field] = value;
        cases.push(invalid);
    }
    for field in ["process_generation", "extension_instance_id", "session_id"] {
        let mut invalid = base.clone();
        invalid["resource_owner"][field] = if field == "process_generation" {
            json!(2)
        } else {
            json!("foreign")
        };
        cases.push(invalid);
    }
    for (index, invalid) in cases.into_iter().enumerate() {
        handle_protocol_line(
            &wave1_line(100 + index as u64, "pi/settings/update", invalid),
            &state,
        )
        .unwrap();
        assert!(response(&mut frames).get("error").is_some());
        assert_eq!(std::fs::read(settings_path(&state)).unwrap(), before);
    }
}

#[test]
fn pi_settings_native_requires_api_feature_binding_and_live_generation() {
    for case in 0..5 {
        let (_directory, mut state, mut frames) = state();
        let request = params(&state, true);
        let path = settings_path(&state);
        let before = std::fs::read(&path).unwrap();
        match case {
            0 => write_std_lock(&state.protocol).features.clear(),
            1 => write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_2.into(),
            2 => state.pi_settings = None,
            3 => state.closed.store(true, Ordering::Release),
            _ => state.draining.store(true, Ordering::Release),
        }
        handle_protocol_line(&wave1_line(100, "pi/settings/update", request), &state).unwrap();
        assert!(response(&mut frames).get("error").is_some());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn pi_settings_native_writer_capacity_and_cancellation_precede_disk_effects() {
    let (_directory, state, mut frames) = state();
    for _ in 0..8 {
        state
            .writer
            .try_send(WriterFrame {
                line: vec![],
                state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                completion: None,
                bus_delivery: None,
            })
            .unwrap();
    }
    let path = settings_path(&state);
    let before = std::fs::read(&path).unwrap();
    assert!(handle_protocol_line(
        &wave1_line(100, "pi/settings/update", params(&state, true)),
        &state,
    )
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    while frames.try_recv().is_ok() {}
    assert!(update(
        &state.pi_settings.as_ref().unwrap().config,
        &[MCP_SELECTOR.into()],
        || true
    )
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn pi_settings_native_read_reports_corruption_and_update_never_overwrites_it() {
    let (_directory, state, mut frames) = state();
    let path = settings_path(&state);
    for (id, bytes) in [
        (100, b"not JSON".as_slice()),
        (102, br#"{"theme":1,"theme":2}"#.as_slice()),
    ] {
        std::fs::write(&path, bytes).unwrap();
        handle_protocol_line(
            &wave1_line(id, "pi/settings/read", params(&state, false)),
            &state,
        )
        .unwrap();
        let read = response(&mut frames);
        assert_eq!(read["result"]["global"], json!({}));
        assert_eq!(read["result"]["errors"][0]["scope"], "global");
        assert_eq!(read["result"]["errors"][0]["path"], json!(path));
        handle_protocol_line(
            &wave1_line(id + 1, "pi/settings/update", params(&state, true)),
            &state,
        )
        .unwrap();
        assert!(response(&mut frames).get("error").is_some());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn pi_settings_native_trusted_project_read_is_explicit_and_false_skips_project_io() {
    let (_directory, mut state, mut frames) = state();
    let config = &mut Arc::get_mut(state.pi_settings.as_mut().unwrap())
        .unwrap()
        .config;
    config.project_trusted = true;
    let project = config.workspace.join(".pi/settings.json");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    std::fs::write(&project, b"not JSON").unwrap();
    handle_protocol_line(
        &wave1_line(100, "pi/settings/read", params(&state, false)),
        &state,
    )
    .unwrap();
    assert_eq!(response(&mut frames)["result"]["errors"], json!([]));
    std::fs::write(&project, br#"{"theme":"project"}"#).unwrap();
    let mut request = params(&state, false);
    request["project_trusted"] = json!(true);
    handle_protocol_line(&wave1_line(101, "pi/settings/read", request), &state).unwrap();
    let read = response(&mut frames);
    assert_eq!(read["result"]["project"], json!({"theme":"project"}));
    assert_eq!(read["result"]["project_trusted"], true);
}

#[cfg(unix)]
#[test]
fn pi_settings_native_symlinks_are_neither_read_nor_written() {
    use std::os::unix::fs::symlink;
    let (_directory, state, mut frames) = state();
    let path = settings_path(&state);
    let outside = path.with_file_name("outside.json");
    std::fs::write(&outside, br#"{"secret":"untouched"}"#).unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();
    handle_protocol_line(
        &wave1_line(100, "pi/settings/read", params(&state, false)),
        &state,
    )
    .unwrap();
    let read = response(&mut frames);
    assert_eq!(read["result"]["global"], json!({}));
    assert_eq!(read["result"]["errors"][0]["scope"], "global");
    handle_protocol_line(
        &wave1_line(101, "pi/settings/update", params(&state, true)),
        &state,
    )
    .unwrap();
    assert!(response(&mut frames).get("error").is_some());
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        br#"{"secret":"untouched"}"#
    );
}

#[test]
fn pi_settings_native_cancelled_origin_stays_revoked_after_tombstone_expiry() {
    let (_directory, state, mut frames) = state();
    let before = std::fs::read(settings_path(&state)).unwrap();
    lock_std_mutex(&state.pending).remove(&7);
    state.pi_settings.as_ref().unwrap().forget_parent(7);
    lock_std_mutex(&state.tombstones).insert(7, Duration::from_secs(60));
    lock_std_mutex(&state.tombstones).remove(7);
    handle_protocol_line(
        &wave1_line(100, "pi/settings/update", params(&state, true)),
        &state,
    )
    .unwrap();
    assert!(response(&mut frames).get("error").is_some());
    assert_eq!(std::fs::read(settings_path(&state)).unwrap(), before);
}

#[test]
fn pi_settings_native_feature_requires_api04_and_native_offer() {
    for (version, offered, allowed) in [
        ("0.4", true, true),
        ("0.4", false, false),
        ("0.2", true, false),
    ] {
        let manifest = ExtensionManifest::parse(&format!("name = \"settings-features\"\nversion = \"0.1.0\"\napi_version = {version:?}\n[entrypoint]\ncommand = \"never-run\"\n")).unwrap();
        let response: InitializeResponse = serde_json::from_value(json!({
            "api_version":version,"tools":[],"commands":[],
            "protocol":{"version":version,"features":["request_cancellation","content_parts",FEATURE],"limits":{"max_concurrent_requests":1}},
        })).unwrap();
        let result = negotiate_contributions_with_host_services(
            &manifest,
            response,
            8,
            OfferedHostServices {
                pi_settings: offered,
                ..OfferedHostServices::default()
            },
        );
        assert_eq!(result.is_ok(), allowed, "{version}/{offered}: {result:?}");
    }
}

#[test]
fn pi_settings_native_bounds_and_concurrent_replacement_refuse_without_lost_updates() {
    let (_directory, state, mut frames) = state();
    let path = settings_path(&state);
    let oversized = vec![b' '; LIMIT + 1];
    std::fs::write(&path, &oversized).unwrap();
    handle_protocol_line(
        &wave1_line(100, "pi/settings/read", params(&state, false)),
        &state,
    )
    .unwrap();
    assert_eq!(
        response(&mut frames)["result"]["errors"][0]["scope"],
        "global"
    );
    handle_protocol_line(
        &wave1_line(101, "pi/settings/update", params(&state, true)),
        &state,
    )
    .unwrap();
    assert!(response(&mut frames).get("error").is_some());
    assert_eq!(std::fs::read(&path).unwrap(), oversized);
    std::fs::write(&path, br#"{"extensions":[],"other":"before"}"#).unwrap();
    let polls = AtomicU64::new(0);
    let concurrent = br#"{"extensions":["concurrent"],"other":"new"}"#;
    assert!(update(
        &state.pi_settings.as_ref().unwrap().config,
        &[MCP_SELECTOR.into()],
        || {
            if polls.fetch_add(1, Ordering::AcqRel) == 1 {
                std::fs::write(&path, concurrent).unwrap();
            }
            false
        }
    )
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), concurrent);
}

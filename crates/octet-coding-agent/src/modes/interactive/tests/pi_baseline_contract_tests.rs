//! Real App/Node acceptance for bounded Pi 1.0.2 baseline contracts (rows 10-12, 15).
//! No synthetic session snapshots or paid providers. Small-history coverage does
//! not remove the session mirror's large-history limit or qualify absent getters.
#![cfg(unix)]
use super::pi_contract_support::{command, pi_app};
use super::support::{scripted_model, seed_compaction_session};
use super::*;
use serde_json::{json, Value};

fn private_entries(session: &Session, kind: &str) -> Vec<(EntryId, Value)> {
    session
        .entries()
        .iter()
        .filter_map(|entry| {
            let private = session.extension_entry(&entry.id, "octet-pi-compat")?;
            (private.entry_type == kind).then_some((entry.id.clone(), private.data))
        })
        .collect()
}

async fn wait_private(app: &mut App, shell: &mut InteractiveShell, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            apply_extension_background(shell, &mut app.executable_extensions);
            app.executable_extensions
                .apply_session_host_requests(&mut app.agent, &app.sessions);
            if let Some((_, data)) = private_entries(app.agent.session(), kind).last() {
                return data.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no native durable {kind} entry: {}", shell.debug_snapshot()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_baseline_flags_defaults_and_host_overrides_are_real_initialized_values() {
    let (_directory, mut app) = pi_app(
        r#"
export default pi => {
  pi.registerFlag('baseline-enabled', { type: 'boolean', default: true });
  pi.registerFlag('baseline-label', { type: 'string', default: 'default-label' });
  pi.registerCommand('flags', { handler: () => pi.appendEntry('flag-values', {
    enabled: pi.getFlag('baseline-enabled'), label: pi.getFlag('baseline-label')
  }) });
};
"#,
    );
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "flags").await.unwrap();
    assert_eq!(
        private_entries(app.agent.session(), "flag-values")[0].1,
        json!({"enabled": true, "label": "default-label"})
    );
    app.config.extension_flag_values.insert(
        "octet-pi-compat".into(),
        [
            ("baseline-enabled".into(), json!(false)),
            ("baseline-label".into(), json!("host-override")),
        ]
        .into_iter()
        .collect(),
    );
    app = rebuild_app(app, None, None, None, None).unwrap();
    command(&mut app, &mut shell, "flags").await.unwrap();
    app.executable_extensions.shutdown().await;
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    let entries = private_entries(&reopened, "flag-values");
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[1].1,
        json!({"enabled": false, "label": "host-override"})
    );
    assert!(reopened.usage_records().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_baseline_shortcut_native_key_dispatch_persists_the_handler_write() {
    let (_directory, mut app) = pi_app(
        r#"
export default pi => {
  pi.registerCommand('prime', { handler: (_args, ctx) => pi.appendEntry('shortcut-prime', {
    session: ctx.sessionManager.getSessionId()
  }) });
  pi.registerShortcut('ctrl+shift+p', { description: 'Baseline shortcut', handler: ctx => {
    pi.appendEntry('shortcut-fired', { session: ctx.sessionManager.getSessionId(), cwd: ctx.cwd });
  }});
};
"#,
    );
    let mut shell = InteractiveShell::test_shell();
    // This real command also drains initialization-time shortcut/register RPCs.
    command(&mut app, &mut shell, "prime").await.unwrap();
    let invocation = app
        .executable_extensions
        .dispatch_shortcut_for_event(&Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('P'),
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
        )))
        .expect("reviewed Pi shortcut is registered with native key routing");
    assert_eq!(invocation.extension, "octet-pi-compat");
    let data = wait_private(&mut app, &mut shell, "shortcut-fired").await;
    app.executable_extensions.shutdown().await;
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(private_entries(&reopened, "shortcut-fired").len(), 1);
    assert_eq!(
        data["session"],
        private_entries(&reopened, "shortcut-prime")[0].1["session"]
    );
    assert_eq!(
        data["cwd"],
        app.config
            .workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert!(reopened.usage_records().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_baseline_event_bus_preserves_identity_order_and_unsubscribe_during_dispatch() {
    let (_directory, mut app) = pi_app(
        r#"
export default pi => {
  pi.registerCommand('bus', { handler: () => {
    const payload = { count: 0, fn: () => 42 };
    const order = [];
    let sameObject = false, sameFunction = false;
    let offSecond;
    const offFirst = pi.events.on('baseline', value => {
      order.push('first'); value.count++; offSecond();
    });
    offSecond = pi.events.on('baseline', value => {
      order.push('second'); sameObject = value === payload; sameFunction = value.fn === payload.fn;
    });
    pi.events.emit('baseline', payload);
    pi.events.emit('baseline', payload);
    offFirst();
    pi.events.emit('baseline', payload);
    pi.appendEntry('bus-observed', { order, count: payload.count, answer: payload.fn(), sameObject, sameFunction });
  }});
};
"#,
    );
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "bus").await.unwrap();
    app.executable_extensions.shutdown().await;
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    let entries = private_entries(&reopened, "bus-observed");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].1,
        json!({"order": ["first", "second", "first"],
        "count": 2, "answer": 42, "sameObject": true, "sameFunction": true})
    );
    assert!(reopened.usage_records().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_baseline_entries_getters_name_and_label_match_reopened_native_session() {
    let (_directory, mut app) = pi_app(
        r#"
export default pi => {
  pi.registerCommand('write', { handler: async () => {
    pi.appendEntry('baseline-state', { value: 17 });
    await pi.setSessionName('Pi baseline name');
  }});
  pi.registerCommand('inspect', { handler: async (_args, ctx) => {
    const entries = ctx.sessionManager.getEntries();
    const state = entries.find(entry => entry.type === 'custom' && entry.customType === 'baseline-state');
    if (!state) throw new Error('durable custom entry missing from actual host snapshot');
    await pi.setLabel(state.id, 'baseline-label');
    const branch = ctx.sessionManager.getBranch();
    pi.appendEntry('getter-observed', {
      state, byId: ctx.sessionManager.getEntry(state.id),
      branchIds: branch.map(entry => entry.id), leaf: ctx.sessionManager.getLeafId(),
      session: ctx.sessionManager.getSessionId(), file: ctx.sessionManager.getSessionFile(),
      name: pi.getSessionName(), managerName: ctx.sessionManager.getSessionName()
    });
  }});
};
"#,
    );
    // The estimate fixture's original JSONL lives outside SessionStore. Use
    // the ordinary managed-session creation path so setSessionName exercises
    // durable native metadata, rather than renaming an untracked test file.
    let path = app.sessions.new_path("20261004-baseline");
    app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::CreateNew(path)),
    )
    .unwrap();
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "write").await.unwrap();
    command(&mut app, &mut shell, "inspect").await.unwrap();
    app.executable_extensions.shutdown().await;
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    let states = private_entries(&reopened, "baseline-state");
    let observed = private_entries(&reopened, "getter-observed");
    assert_eq!(states.len(), 1);
    assert_eq!(observed.len(), 1);
    assert_eq!(states[0].1, json!({"value": 17}));
    let data = &observed[0].1;
    assert_eq!(data["state"]["id"], states[0].0 .0);
    assert_eq!(data["state"]["data"], states[0].1);
    assert_eq!(data["state"], data["byId"]);
    assert!(data["branchIds"]
        .as_array()
        .unwrap()
        .contains(&json!(states[0].0 .0)));
    assert_eq!(data["leaf"], states[0].0 .0);
    assert_eq!(data["file"], reopened.path().to_string_lossy().as_ref());
    assert_eq!(
        data["session"],
        reopened.path().file_stem().unwrap().to_str().unwrap()
    );
    assert_eq!(data["name"], "Pi baseline name");
    assert_eq!(data["managerName"], "Pi baseline name");
    assert_eq!(reopened.entry_label(&states[0].0), Some("baseline-label"));
    assert_eq!(
        app.sessions
            .load_metadata(data["session"].as_str().unwrap())
            .unwrap()
            .name
            .as_deref(),
        Some("Pi baseline name")
    );
    assert!(reopened.usage_records().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_baseline_compaction_request_callback_and_tree_hooks_have_durable_native_evidence() {
    let (_directory, mut app) = pi_app(
        r#"
export default pi => {
  pi.on('session_before_compact', event => {
    pi.appendEntry('compact-before', { reason: event.reason, instructions: event.customInstructions });
    return { compaction: { summary: 'PI_BASELINE_SUMMARY', firstKeptEntryId: event.preparation.firstKeptEntryId } };
  });
  pi.on('session_compact', event => pi.appendEntry('compact-after', {
    entry: event.compactionEntry.id, summary: event.compactionEntry.summary, fromExtension: event.fromExtension
  }));
  pi.on('session_before_tree', event => pi.appendEntry('tree-before', {
    target: event.preparation.targetId, old: event.preparation.oldLeafId
  }));
  pi.on('session_tree', event => pi.appendEntry('tree-after', { old: event.oldLeafId, new: event.newLeafId }));
  pi.registerCommand('baseline-compact', { handler: (_args, ctx) => {
    const result = ctx.compact({ customInstructions: 'baseline instructions',
      onComplete: result => pi.appendEntry('compact-callback', result),
      onError: error => pi.appendEntry('compact-error', { error: String(error) }) });
    pi.appendEntry('compact-void', { isVoid: result === undefined });
  }});
};
"#,
    );
    // Binding the real Agent hooks requires the ordinary native App rebuild.
    // Even a broken interception path can only contact this local test server.
    let server = wiremock::MockServer::start().await;
    let model = scripted_model(&server.uri());
    app.catalog
        .register_endpoint((*model.endpoint).clone())
        .unwrap();
    app.catalog.register_model((*model.spec).clone()).unwrap();
    app.config.compaction.keep_recent_tokens = 1;
    app = rebuild_app(app, Some(model), None, None, None).unwrap();
    // Rebuild intentionally leaves lifecycle work inactive until the real
    // interactive idle consumer takes ownership of the replacement App.
    app.executable_extensions
        .activate_session_lifecycle_driver();
    seed_compaction_session(&mut app.agent);
    app.agent
        .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, 1)
        .unwrap();
    let target = app
        .agent
        .session()
        .entries()
        .iter()
        .find(|entry| {
            matches!(
                &entry.value,
                EntryValue::Message(octet_ai::Message::User(_))
            )
        })
        .unwrap()
        .id
        .clone();
    let mut shell = InteractiveShell::test_shell();
    command(&mut app, &mut shell, "baseline-compact")
        .await
        .unwrap();
    assert_eq!(
        private_entries(app.agent.session(), "compact-void")[0].1,
        json!({"isVoid": true})
    );
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    // The production command pump can already execute retained compaction
    // after the wire reply settles. Await durable completion, not a second
    // dequeue of the same request. Both legitimate idle schedules are covered.
    let callback = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            apply_extension_background(&mut shell, &mut app.executable_extensions);
            if let Some(request) = app.executable_extensions.next_session_lifecycle_request() {
                execute_extension_session_lifecycle(&mut app, &mut shell, &mut input, request)
                    .await;
            }
            let callbacks = private_entries(app.agent.session(), "compact-callback");
            if let Some((_, value)) = callbacks.first() {
                assert_eq!(callbacks.len(), 1);
                break value.clone();
            }
            assert!(private_entries(app.agent.session(), "compact-error").is_empty());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Pi ctx.compact must commit and callback after its command replies");
    assert_eq!(callback["summary"], "PI_BASELINE_SUMMARY");
    assert!(private_entries(app.agent.session(), "compact-error").is_empty());
    let old_head = app.agent.session().head().unwrap();
    let navigation = app.agent.navigate_session_tree(
        Some(target.clone()),
        octet_agent::CancellationToken::default(),
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        await_with_ctrl_c_and_extensions(
            navigation,
            &mut shell,
            &mut input,
            Some(&mut app.executable_extensions),
        ),
    )
    .await
    .expect("native tree hook operation timed out")
    .expect("tree operation interrupted")
    .unwrap();
    app.executable_extensions.shutdown().await;
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    let compacted = reopened.entries().iter().filter(|entry|
        matches!(&entry.value, EntryValue::Compaction { summary, .. } if summary == "PI_BASELINE_SUMMARY"))
        .collect::<Vec<_>>();
    assert_eq!(compacted.len(), 1);
    assert_eq!(private_entries(&reopened, "compact-callback").len(), 1);
    assert_eq!(private_entries(&reopened, "compact-before").len(), 1);
    assert_eq!(private_entries(&reopened, "compact-after").len(), 1);
    assert_eq!(
        private_entries(&reopened, "compact-before")[0].1,
        json!({"reason": "manual", "instructions": "baseline instructions"})
    );
    assert_eq!(
        private_entries(&reopened, "compact-after")[0].1,
        json!({"entry": compacted[0].id.0, "summary": "PI_BASELINE_SUMMARY", "fromExtension": true})
    );
    let EntryValue::Compaction { first_kept, .. } = &compacted[0].value else {
        unreachable!()
    };
    assert_eq!(callback["firstKeptEntryId"], first_kept.0);
    assert_eq!(
        private_entries(&reopened, "tree-before")[0].1,
        json!({"target": target.0, "old": old_head.0})
    );
    let after = private_entries(&reopened, "tree-after");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].1, json!({"old": old_head.0, "new": target.0}));
    assert_eq!(reopened.entry(&after[0].0).unwrap().parent, Some(target));
    assert_eq!(reopened.head(), Some(after[0].0.clone()));
    assert!(reopened.usage_records().is_empty());
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "replacement must bypass inference entirely"
    );
}

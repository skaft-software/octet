use super::*;

async fn admit(state: &mut ProbeState, shell: &mut InteractiveShell) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            state.extensions.drain_events_for_shell(shell);
            if !state.extensions.autocomplete_registrations.is_empty() {
                break;
            }
            // A cooperative wait lets the process reader and the child run; a
            // yield-only spin starves both when the host is loaded.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("real autocomplete registration");
}

async fn update(state: &mut ProbeState) -> ExtensionAutocompleteUpdate {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = state.extensions.drain_background_updates();
            if let Some(update) = updates.autocomplete.into_iter().next() {
                return update;
            }
            // A cooperative wait lets the process reader and the child run; a
            // yield-only spin starves both when the host is loaded.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("completion chain finished")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unclaimed_autocomplete_preserves_native_paths_and_rejects_stale_fallback() {
    let mut state = start_probe("empty").await;
    std::fs::write(state._root.path().join("file.rs"), "source").unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(state._root.path().to_path_buf());
    admit(&mut state, &mut shell).await;

    let snapshot = shell.extension_set_editor("./fi".into());
    assert!(state
        .extensions
        .request_editor_autocomplete(snapshot.clone()));
    let response = update(&mut state).await;
    assert!(response.items.is_empty());
    assert_eq!(recorded(&state.process).await["complete"]["text"], "./fi");
    assert!(state
        .extensions
        .set_editor_autocomplete(&mut shell, response));
    assert_eq!(shell.pending(), "./file.rs ");

    let snapshot = shell.extension_set_editor("./fi".into());
    assert!(state
        .extensions
        .request_editor_autocomplete(snapshot.clone()));
    // Deterministic edit before consuming even an already-queued reply; no sleep
    // or luck deciding whether a stale fallback mutates the new draft.
    shell.extension_set_editor("new draft".into());
    let response = update(&mut state).await;
    assert!(!state
        .extensions
        .set_editor_autocomplete(&mut shell, response));
    assert_eq!(shell.pending(), "new draft");

    // Returning to identical bytes does not revive an earlier revision.
    shell.extension_set_editor(snapshot.text.clone());
    assert!(!shell.set_extension_autocomplete(&snapshot, String::new(), Vec::new()));
    assert_eq!(shell.pending(), "./fi");
    let mut unfocused = shell.extension_editor_snapshot();
    unfocused.focused = false;
    assert!(!shell.set_extension_autocomplete(&unfocused, String::new(), Vec::new()));
    assert_eq!(shell.pending(), "./fi");
    state.extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_autocomplete_chain_delegates_to_next_real_peer() {
    let mut empty = start_named_probe("empty", "a-empty").await;
    let mut positive = start_named_probe("ui", "b-positive").await;
    let mut shell = InteractiveShell::test_shell();
    admit(&mut empty, &mut shell).await;
    admit(&mut positive, &mut shell).await;
    let first = empty
        .extensions
        .autocomplete_registrations
        .values()
        .next()
        .unwrap()
        .clone();
    let second = positive
        .extensions
        .autocomplete_registrations
        .values()
        .next()
        .unwrap()
        .clone();
    empty.extensions.processes.push(positive.process.clone());
    empty.extensions.autocomplete_registrations =
        BTreeMap::from([("a-empty".into(), first), ("b-positive".into(), second)]);
    let snapshot = shell.extension_set_editor("@fi".into());
    assert!(empty.extensions.request_editor_autocomplete(snapshot));
    let response = update(&mut empty).await;
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].value, "file.rs");
    for peer in [&empty.process, &positive.process] {
        assert_eq!(recorded(peer).await["complete"]["text"], "@fi");
    }
    assert!(empty
        .extensions
        .set_editor_autocomplete(&mut shell, response));
    assert!(empty.extensions.accept_editor_autocomplete(&mut shell));
    assert_eq!(shell.pending(), "@file.rs");
    empty.extensions.shutdown().await;
    positive.extensions.shutdown().await;
}

fn quoted_snapshot(shell: &mut InteractiveShell, text: &str) -> ShellEditorSnapshot {
    shell.extension_set_editor(text.into());
    shell.apply_edit(crate::tui::keymap::EditAction::Left);
    shell.extension_editor_snapshot()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autocomplete_edit_real_peer_consumes_quote_and_places_unicode_directory_cursor() {
    let mut state = start_probe("edit").await;
    let mut shell = InteractiveShell::test_shell();
    admit(&mut state, &mut shell).await;
    for (original, expected) in [("é\"", "文/\""), ("\n\t\ré\"", "文\n\t\r/\"")] {
        let snapshot = quoted_snapshot(&mut shell, original);
        assert!(state
            .extensions
            .request_editor_autocomplete(snapshot.clone()));
        let response = update(&mut state).await;
        assert_eq!(response.items[0].replace_after_bytes, Some(1));
        assert_eq!(
            response.items[0].cursor_offset_bytes,
            Some((expected.len() - 1) as u32)
        );
        assert!(state
            .extensions
            .set_editor_autocomplete(&mut shell, response));
        assert_eq!(shell.pending(), original, "display must not apply the edit");
        assert!(state.extensions.accept_editor_autocomplete(&mut shell));
        let applied = shell.extension_editor_snapshot();
        assert_eq!(applied.text, expected);
        assert_eq!(
            applied.cursor,
            expected.len() - 1,
            "cursor remains before closing quote"
        );
        assert!(applied.revision > snapshot.revision);
        assert!(applied.focused);
        assert!(!state.extensions.accept_editor_autocomplete(&mut shell));
    }
    eprintln!(
        "native autocomplete edit peer: {}",
        recorded(&state.process).await
    );
    state.extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autocomplete_edit_real_peer_fences_text_cursor_revision_and_focus_at_both_boundaries() {
    let mut state = start_probe("edit").await;
    let mut shell = InteractiveShell::test_shell();
    admit(&mut state, &mut shell).await;
    for displayed in [false, true] {
        for change in ["text", "cursor", "revision", "focus"] {
            shell.set_tool_input_prompt(None);
            let snapshot = quoted_snapshot(&mut shell, "é\"");
            assert!(state.extensions.request_editor_autocomplete(snapshot));
            // Awaiting this response is the explicit response-ready barrier.
            let response = update(&mut state).await;
            if displayed {
                assert!(state
                    .extensions
                    .set_editor_autocomplete(&mut shell, response.clone()));
            }
            use crate::tui::keymap::EditAction;
            match change {
                "text" => {
                    shell.extension_set_editor("new draft".into());
                }
                "cursor" => shell.apply_edit(EditAction::Left),
                "revision" => {
                    shell.apply_edit(EditAction::Right);
                    shell.apply_edit(EditAction::Left);
                }
                "focus" => shell.set_tool_input_prompt(Some("host-owned input".into())),
                _ => unreachable!(),
            }
            let before = shell.extension_editor_snapshot();
            if !displayed {
                assert!(!state
                    .extensions
                    .set_editor_autocomplete(&mut shell, response));
            }
            assert!(
                !state.extensions.accept_editor_autocomplete(&mut shell),
                "{displayed}/{change}"
            );
            assert_eq!(shell.extension_editor_snapshot(), before);
        }
    }
    eprintln!(
        "native autocomplete stale editor peer: {}",
        recorded(&state.process).await
    );
    state.extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autocomplete_edit_real_peer_fences_retirement_reload_instance_and_session_at_both_boundaries(
) {
    for displayed in [false, true] {
        for change in ["retirement", "reload", "instance", "session"] {
            let mut state = start_probe("edit").await;
            let mut shell = InteractiveShell::test_shell();
            admit(&mut state, &mut shell).await;
            let snapshot = quoted_snapshot(&mut shell, "é\"");
            assert!(state
                .extensions
                .request_editor_autocomplete(snapshot.clone()));
            let response = update(&mut state).await;
            let wire = recorded(&state.process).await;
            if displayed {
                assert!(state
                    .extensions
                    .set_editor_autocomplete(&mut shell, response.clone()));
            }
            // All lifecycle changes complete strictly after the response-ready
            // barrier, and (when requested) after the actual native display.
            let mut replacement = None;
            match change {
                "retirement" => {
                    assert!(state.process.shutdown().await);
                }
                "reload" => {
                    let generation = state.process.health_snapshot().generation;
                    state.process.reload().await.unwrap();
                    assert!(state.process.health_snapshot().generation > generation);
                    state.extensions.prune_semantic_ui();
                    admit(&mut state, &mut shell).await;
                }
                "instance" => {
                    let mut next = start_probe("edit").await;
                    admit(&mut next, &mut shell).await;
                    assert_ne!(
                        state.process.extension_instance_id(),
                        next.process.extension_instance_id()
                    );
                    assert_eq!(
                        state.process.health_snapshot().generation,
                        next.process.health_snapshot().generation
                    );
                    let registration =
                        next.extensions.autocomplete_registrations[PROBE_EXTENSION].clone();
                    state.extensions.processes.push(next.process.clone());
                    state
                        .extensions
                        .autocomplete_registrations
                        .insert(PROBE_EXTENSION.into(), registration);
                    replacement = Some(next);
                }
                "session" => {
                    let owner = state.extensions.resource_owner.clone();
                    let session =
                        Session::create(state._root.path().join("replacement.jsonl")).unwrap();
                    let model = ModelCatalog::builtin()
                        .unwrap()
                        .resolve(&ModelId("gpt-4o-mini".into()))
                        .unwrap();
                    let sessions =
                        SessionStore::new(&state._root.path().join("sessions"), state._root.path());
                    state.extensions.transition_active_session(
                        &session,
                        &model,
                        &ReasoningConfig::Off,
                        &sessions,
                    );
                    assert_ne!(state.extensions.resource_owner, owner);
                }
                _ => unreachable!(),
            }
            if displayed {
                assert!(
                    state.extensions.reconcile_editor_autocomplete(&mut shell),
                    "{change}"
                );
                assert!(state.extensions.displayed_autocomplete.is_none());
            } else {
                assert!(
                    !state
                        .extensions
                        .set_editor_autocomplete(&mut shell, response),
                    "{change}"
                );
            }
            assert!(
                !state.extensions.accept_editor_autocomplete(&mut shell),
                "{change}"
            );
            assert_eq!(shell.extension_editor_snapshot(), snapshot, "{change}");
            eprintln!("native autocomplete lifecycle {displayed}/{change}: {wire}");
            state.extensions.shutdown().await;
            if let Some(mut next) = replacement {
                next.extensions.shutdown().await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unclaimed_real_peer_retirement_cannot_trigger_native_fallback() {
    let mut state = start_probe("empty").await;
    std::fs::write(state._root.path().join("file.rs"), "source").unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(state._root.path().to_path_buf());
    admit(&mut state, &mut shell).await;
    let snapshot = shell.extension_set_editor("./fi".into());
    assert!(state
        .extensions
        .request_editor_autocomplete(snapshot.clone()));
    let response = update(&mut state).await;
    assert!(response.items.is_empty());
    assert!(state.process.shutdown().await);
    assert!(!state
        .extensions
        .set_editor_autocomplete(&mut shell, response));
    assert_eq!(shell.extension_editor_snapshot(), snapshot);
    state.extensions.shutdown().await;
}

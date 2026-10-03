//! Invoking the hosted `gh` binary on behalf of a session.
//! Everything here is about the process boundary: resolving the executable
//! without trusting a workspace-local fake, scrubbing provider canaries out of
//! the child environment, bounding the JSON we accept, killing descendants on
//! timeout, and streaming an authoritative refresh into the catalog.

use super::*;

// Every test here drives a Unix `gh` fixture.
#[cfg(unix)]
use super::test_support::*;

#[cfg(unix)]
#[tokio::test]
async fn github_cli_resolver_ignores_relative_and_workspace_local_fakes() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let workspace_bin = workspace.join("bin");
    let workspace_alias = directory.path().join("workspace-bin-alias");
    let external_bin = directory.path().join("external-bin");
    std::fs::create_dir_all(&workspace_bin).unwrap();
    std::fs::create_dir_all(&external_bin).unwrap();
    symlink(&workspace_bin, &workspace_alias).unwrap();

    let write_executable = |path: &Path, body: &str| {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    write_executable(
        &workspace_bin.join("gh"),
        r#"#!/bin/sh
printf workspace > workspace-gh-ran
printf '%s' '{"number":111,"url":"https://github.com/skaft-software/ygg/pull/111","state":"OPEN","isDraft":false}'
"#,
    );
    write_executable(
        &external_bin.join("gh"),
        r#"#!/bin/sh
printf external > external-gh-ran
printf '%s' '{"number":124,"url":"https://github.com/skaft-software/ygg/pull/124","state":"OPEN","isDraft":false}'
"#,
    );
    let path = std::env::join_paths([
        OsString::from("relative-bin"),
        workspace_bin.as_os_str().to_owned(),
        workspace_alias.as_os_str().to_owned(),
        external_bin.as_os_str().to_owned(),
    ])
    .unwrap();

    let resolved = resolve_github_cli_executable_from_path(&workspace, &path).unwrap();
    assert_eq!(resolved, external_bin.canonicalize().unwrap().join("gh"));
    assert_eq!(
        query_github_pull_request(&workspace, None, &resolved).await,
        PullRequestObservation::Trackable {
            number: 124,
            url: "https://github.com/skaft-software/ygg/pull/124".into(),
            state: PullRequestState::Ready,
        }
    );
    assert!(!workspace.join("workspace-gh-ran").exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join("external-gh-ran")).unwrap(),
        "external"
    );
}

#[test]
fn github_cli_environment_retains_auth_and_filters_provider_canaries() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let workspace_bin = workspace.join("bin");
    let external_bin = directory.path().join("external-bin");
    std::fs::create_dir_all(&workspace_bin).unwrap();
    std::fs::create_dir_all(&external_bin).unwrap();
    let path = std::env::join_paths([
        OsString::from("relative-bin"),
        workspace_bin.as_os_str().to_owned(),
        external_bin.as_os_str().to_owned(),
    ])
    .unwrap();
    let values = BTreeMap::from([
        (OsString::from("GH_TOKEN"), OsString::from("gh-token")),
        (
            OsString::from("GITHUB_TOKEN"),
            OsString::from("github-token"),
        ),
        (
            OsString::from("GH_ENTERPRISE_TOKEN"),
            OsString::from("enterprise-token"),
        ),
        (OsString::from("GH_HOST"), OsString::from("github.example")),
        (
            OsString::from("HTTPS_PROXY"),
            OsString::from("https://proxy"),
        ),
        (
            OsString::from("SSL_CERT_FILE"),
            OsString::from("/etc/reviewed-ca.pem"),
        ),
        (OsString::from("LANG"), OsString::from("C.UTF-8")),
        (OsString::from("HOME"), OsString::from("/home/reviewer")),
        (
            OsString::from("OPENAI_API_KEY"),
            OsString::from("provider-canary"),
        ),
        (
            OsString::from("AWS_SECRET_ACCESS_KEY"),
            OsString::from("provider-canary"),
        ),
        (
            OsString::from("OCTET_PROVIDER_CANARY"),
            OsString::from("provider-canary"),
        ),
        (OsString::from("LD_PRELOAD"), OsString::from("/tmp/evil.so")),
        (
            OsString::from("GH_REPO"),
            OsString::from("wrong/repository"),
        ),
    ]);
    let environment = github_cli_environment_from(
        &workspace,
        |name| values.get(OsStr::new(name)).cloned(),
        Some(path.as_os_str()),
    );

    assert_eq!(
        environment
            .get(OsStr::new("GH_TOKEN"))
            .and_then(|value| value.to_str()),
        Some("gh-token")
    );
    assert_eq!(
        environment
            .get(OsStr::new("GITHUB_TOKEN"))
            .and_then(|value| value.to_str()),
        Some("github-token")
    );
    assert_eq!(
        environment
            .get(OsStr::new("GH_ENTERPRISE_TOKEN"))
            .and_then(|value| value.to_str()),
        Some("enterprise-token")
    );
    assert_eq!(
        environment
            .get(OsStr::new("HTTPS_PROXY"))
            .and_then(|value| value.to_str()),
        Some("https://proxy")
    );
    assert_eq!(
        environment
            .get(OsStr::new("SSL_CERT_FILE"))
            .and_then(|value| value.to_str()),
        Some("/etc/reviewed-ca.pem")
    );
    assert_eq!(
        std::env::split_paths(environment.get(OsStr::new("PATH")).unwrap()).collect::<Vec<_>>(),
        vec![external_bin.canonicalize().unwrap()]
    );
    for name in [
        "OPENAI_API_KEY",
        "AWS_SECRET_ACCESS_KEY",
        "OCTET_PROVIDER_CANARY",
        "LD_PRELOAD",
        "GH_REPO",
    ] {
        assert!(!environment.contains_key(OsStr::new(name)), "leaked {name}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn github_cli_fixture_receives_auth_without_provider_canaries() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let external_bin = directory.path().join("external-bin");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&external_bin).unwrap();
    let executable = external_bin.join("gh");
    std::fs::write(
        &executable,
        r#"#!/bin/sh
if [ "${GH_TOKEN-}" != "gh-token" ] || [ "${GH_ENTERPRISE_TOKEN-}" != "enterprise-token" ]; then
  exit 21
fi
if [ -n "${OPENAI_API_KEY-}" ] || [ -n "${AWS_SECRET_ACCESS_KEY-}" ] || [ -n "${OCTET_PROVIDER_CANARY-}" ] || [ -n "${LD_PRELOAD-}" ] || [ -n "${GH_REPO-}" ]; then
  exit 22
fi
printf '%s' '{"number":124,"url":"https://github.com/skaft-software/ygg/pull/124","state":"OPEN","isDraft":false}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths([external_bin.as_os_str()]).unwrap();
    let values = BTreeMap::from([
        (OsString::from("GH_TOKEN"), OsString::from("gh-token")),
        (
            OsString::from("GH_ENTERPRISE_TOKEN"),
            OsString::from("enterprise-token"),
        ),
        (
            OsString::from("OPENAI_API_KEY"),
            OsString::from("provider-canary"),
        ),
        (
            OsString::from("AWS_SECRET_ACCESS_KEY"),
            OsString::from("provider-canary"),
        ),
        (
            OsString::from("OCTET_PROVIDER_CANARY"),
            OsString::from("provider-canary"),
        ),
        (OsString::from("LD_PRELOAD"), OsString::from("/tmp/evil.so")),
        (
            OsString::from("GH_REPO"),
            OsString::from("wrong/repository"),
        ),
    ]);
    let environment = github_cli_environment_from(
        &workspace,
        |name| values.get(OsStr::new(name)).cloned(),
        Some(path.as_os_str()),
    );

    assert_eq!(
        execute_github_pull_request_query_with_environment(
            &workspace,
            None,
            &executable,
            std::time::Duration::from_secs(1),
            &environment,
        )
        .await,
        PullRequestObservation::Trackable {
            number: 124,
            url: "https://github.com/skaft-software/ygg/pull/124".into(),
            state: PullRequestState::Ready,
        }
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pull_request_refresh_honors_no_process_for_periodic_and_requested_work() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let mut plan = pull_request_worker_plan(directory.path(), "no-process-refresh");
    plan.config.sandbox.allow_process = false;
    plan.pull_request_discovery_enabled
        .store(true, Ordering::Release);
    let executable = directory.path().join("gh-no-process-fixture");
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf spawned > no-process-gh-ran\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (events, mut received) = mpsc::channel(4);
    let refresh_plan = PullRequestRefreshPlan::from(&plan);
    assert!(!refresh_plan.process_execution_allowed);
    let request = Arc::clone(&refresh_plan.refresh_requested);
    let task = tokio::spawn(run_hosted_pull_request_refresh(
        refresh_plan,
        events.clone(),
    ));
    request.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();

    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert!(!plan.config.workspace.join("no-process-gh-ran").exists());
    assert_eq!(
        plan.pull_requests.lock().unwrap().summary(&plan.session_id),
        None
    );
    assert!(received.try_recv().is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn hosted_pull_request_refresh_retries_and_streams_authoritative_transitions() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("gh-hosted-fixture");
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":124,\"url\":\"https://github.com/skaft-software/ygg/pull/124\",\"state\":\"OPEN\",\"isDraft\":true}'\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

    let hosted_directory = directory.path().join("hosted");
    let plan = pull_request_worker_plan(&hosted_directory, "hosted-pull-request-refresh");
    let session_id = plan.session_id.clone();
    let (events, mut received) = mpsc::channel(8);

    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert_eq!(
        plan.pull_requests.lock().unwrap().summary(&session_id),
        None
    );
    assert!(received.try_recv().is_err());

    plan.pull_request_discovery_enabled
        .store(true, Ordering::Release);
    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert!(matches!(
        received.recv().await.unwrap().payload,
        EventPayload::SessionPullRequestChanged {
            pull_request: Some(PullRequestSummary {
                state: PullRequestState::InProgress
            })
        }
    ));

    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":124,\"url\":\"https://github.com/skaft-software/ygg/pull/124\",\"state\":\"OPEN\",\"isDraft\":false}'\n",
        ),
    )
    .unwrap();
    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert!(matches!(
        received.recv().await.unwrap().payload,
        EventPayload::SessionPullRequestChanged {
            pull_request: Some(PullRequestSummary {
                state: PullRequestState::Ready
            })
        }
    ));

    std::fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert_eq!(
        plan.pull_requests.lock().unwrap().summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );
    assert!(received.try_recv().is_err());

    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":124,\"url\":\"https://github.com/skaft-software/ygg/pull/124\",\"state\":\"MERGED\",\"isDraft\":false}'\n",
        ),
    )
    .unwrap();
    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert!(matches!(
        received.recv().await.unwrap().payload,
        EventPayload::SessionPullRequestChanged {
            pull_request: Some(PullRequestSummary {
                state: PullRequestState::Merged
            })
        }
    ));

    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":124,\"url\":\"https://github.com/skaft-software/ygg/pull/124\",\"state\":\"CLOSED\",\"isDraft\":false}'\n",
        ),
    )
    .unwrap();
    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert_eq!(
        plan.pull_requests.lock().unwrap().summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Merged,
        })
    );
    assert!(received.try_recv().is_err());
    drop(plan);
    assert_eq!(
        PullRequestStore::open(&hosted_directory.join("state"))
            .unwrap()
            .summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Merged,
        })
    );

    let closed_directory = directory.path().join("closed");
    let plan = pull_request_worker_plan(&closed_directory, "closed-pull-request-refresh");
    plan.pull_request_discovery_enabled
        .store(true, Ordering::Release);
    let session_id = plan.session_id.clone();
    let (events, mut received) = mpsc::channel(4);
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":125,\"url\":\"https://github.com/skaft-software/ygg/pull/125\",\"state\":\"OPEN\",\"isDraft\":false}'\n",
        ),
    )
    .unwrap();
    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    let _ = received.recv().await.unwrap();
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":125,\"url\":\"https://github.com/skaft-software/ygg/pull/125\",\"state\":\"CLOSED\",\"isDraft\":false}'\n",
        ),
    )
    .unwrap();
    refresh_pull_request_projection_with_executable(&plan, &events, &executable)
        .await
        .unwrap();
    assert!(matches!(
        received.recv().await.unwrap().payload,
        EventPayload::SessionPullRequestChanged { pull_request: None }
    ));
    assert_eq!(
        plan.pull_requests.lock().unwrap().summary(&session_id),
        None
    );
    drop(plan);
    assert_eq!(
        PullRequestStore::open(&closed_directory.join("state"))
            .unwrap()
            .summary(&session_id),
        None
    );
}

#[cfg(unix)]
#[tokio::test]
async fn github_cli_query_accepts_only_successful_bounded_json() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("gh-fixture");
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":124,\"url\":\"https://github.com/skaft-software/ygg/pull/124\",\"state\":\"OPEN\",\"isDraft\":false}'\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        query_github_pull_request(
            directory.path(),
            Some("https://github.com/skaft-software/ygg/pull/124"),
            &executable,
        )
        .await,
        PullRequestObservation::Trackable {
            number: 124,
            url: "https://github.com/skaft-software/ygg/pull/124".into(),
            state: PullRequestState::Ready,
        }
    );

    let queued_permits = Arc::new(tokio::sync::Semaphore::new(0));
    let queued_query = {
        let workspace = directory.path().to_owned();
        let executable = executable.clone();
        let permits = Arc::clone(&queued_permits);
        tokio::spawn(async move {
            query_github_pull_request_with_timeout_and_queued_permit(
                &workspace,
                None,
                &executable,
                std::time::Duration::from_secs(1),
                &permits,
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!queued_query.is_finished());
    queued_permits.add_permits(1);
    assert_eq!(
        queued_query.await.unwrap(),
        PullRequestObservation::Trackable {
            number: 124,
            url: "https://github.com/skaft-software/ygg/pull/124".into(),
            state: PullRequestState::Ready,
        }
    );

    std::fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
    assert_eq!(
        query_github_pull_request(directory.path(), None, &executable).await,
        PullRequestObservation::Unavailable
    );

    std::fs::write(
        &executable,
        "#!/bin/sh\ni=0\nwhile [ $i -lt 20000 ]; do printf x; i=$((i + 1)); done\n",
    )
    .unwrap();
    assert_eq!(
        query_github_pull_request(directory.path(), None, &executable).await,
        PullRequestObservation::Unavailable
    );

    std::fs::write(&executable, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        query_github_pull_request_with_timeout(
            directory.path(),
            None,
            &executable,
            std::time::Duration::from_millis(30),
        )
        .await,
        PullRequestObservation::Unavailable
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));

    let saturated = tokio::sync::Semaphore::new(0);
    let started = std::time::Instant::now();
    assert_eq!(
        query_github_pull_request_with_timeout_and_permits(
            directory.path(),
            None,
            &executable,
            std::time::Duration::from_secs(1),
            &saturated,
        )
        .await,
        PullRequestObservation::Unavailable
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
}

#[cfg(unix)]
#[tokio::test]
async fn github_cli_query_timeout_kills_background_descendants() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("gh-descendant-fixture");
    let descendant_pid = directory.path().join("descendant.pid");
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "/bin/sh -c 'trap \"\" TERM; printf \"%s\\n\" \"$$\" > descendant.pid; while :; do /bin/sleep 1; done' &\n",
            "while :; do /bin/sleep 1; done\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Use private admission and poll once to start the owned process. Do
    // not poll the query again until the descendant has installed its TERM
    // trap: cold executable startup can exceed the query timeout under load.
    // Keeping the future local also retains its drop cleanup on test panic.
    let permits = tokio::sync::Semaphore::new(1);
    let timeout = std::time::Duration::from_millis(500);
    let query = query_github_pull_request_with_timeout_and_permits(
        directory.path(),
        None,
        &executable,
        timeout,
        &permits,
    );
    tokio::pin!(query);
    assert!(
        futures_util::poll!(&mut query).is_pending(),
        "GitHub query completed before its fixture started"
    );
    let query_deadline = tokio::time::Instant::now() + timeout;
    let startup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(&descendant_pid)
            .ok()
            .filter(|text| text.ends_with('\n'))
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            break pid;
        }
        assert!(
            std::time::Instant::now() < startup_deadline,
            "GitHub query descendant did not publish readiness"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    let process_exists = |pid: i32| {
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    };
    assert!(process_exists(pid), "GitHub query descendant exited early");

    // Expire the real query timer before resuming it, then measure cleanup
    // independently of OS startup. The production timeout path is unchanged.
    tokio::time::sleep_until(query_deadline).await;
    let started = std::time::Instant::now();
    assert_eq!(query.await, PullRequestObservation::Unavailable);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "GitHub query cleanup exceeded its bound: {:?}",
        started.elapsed()
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while process_exists(pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !process_exists(pid),
        "GitHub query descendant survived timeout cleanup"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn inactive_pull_request_refresh_updates_the_persisted_catalog_stream() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, _, _, _) =
        worker_checkout_fixture(directory.path(), "inactive-pull-request-refresh");
    host.pull_requests
        .lock()
        .unwrap()
        .replace(
            &session_id,
            Some(stored_pull_request(
                &session_id,
                124,
                PullRequestState::Ready,
            )),
        )
        .unwrap();
    let host = Arc::new(host);
    let supervisor = Arc::new(SessionSupervisor::new(
        Arc::clone(&host),
        SupervisorConfig::default(),
    ));
    let mut events = supervisor.subscribe_events();
    let executable = directory.path().join("gh-refresh-fixture");
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '{\"number\":124,\"url\":\"https://github.com/skaft-software/ygg/pull/124\",\"state\":\"MERGED\",\"isDraft\":false}'\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

    let mut pending = BTreeSet::new();
    let mut attempted = BTreeSet::new();
    refresh_inactive_pull_requests_once(
        &host,
        &supervisor,
        &mut pending,
        &mut attempted,
        &executable,
    )
    .await;

    assert!(pending.is_empty());
    assert_eq!(
        host.pull_requests.lock().unwrap().summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Merged,
        })
    );
    let streamed = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    let catalog = streamed.catalog.expect("catalog refresh");
    assert_eq!(catalog.summary.id, session_id);
    assert_eq!(
        catalog.summary.pull_request,
        Some(PullRequestSummary {
            state: PullRequestState::Merged,
        })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn inactive_catalog_reconciles_a_terminal_hosted_store_handoff() {
    let directory = tempfile::tempdir().unwrap();
    let (host, session_id, _, _, _) =
        worker_checkout_fixture(directory.path(), "terminal-pull-request-handoff");
    host.pull_requests
        .lock()
        .unwrap()
        .replace(
            &session_id,
            Some(stored_pull_request(
                &session_id,
                124,
                PullRequestState::Merged,
            )),
        )
        .unwrap();
    let host = Arc::new(host);
    let supervisor = Arc::new(SessionSupervisor::new(
        Arc::clone(&host),
        SupervisorConfig::default(),
    ));
    let mut events = supervisor.subscribe_events();

    let mut pending = BTreeSet::new();
    let mut attempted = BTreeSet::new();
    refresh_inactive_pull_requests_once(
        &host,
        &supervisor,
        &mut pending,
        &mut attempted,
        Path::new("unused-gh"),
    )
    .await;

    assert!(pending.is_empty());
    let streamed = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        streamed
            .catalog
            .expect("catalog handoff")
            .summary
            .pull_request,
        Some(PullRequestSummary {
            state: PullRequestState::Merged,
        })
    );
}

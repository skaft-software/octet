//! Which commands the host advertises, and when it may run one.
//! The builder cases check that an extension cannot smuggle a name past a
//! builtin prefix and that oversized metadata is trimmed for the transport; the
//! actor cases check that a stream event for an unknown command is buffered
//! rather than dropped, and that an idle command runs off the prompt path.

use super::*;
use octet_serve_backend::PROTOCOL_VERSION;

use super::test_support::*;

#[test]
fn command_discovery_hides_dynamic_names_claimed_by_builtin_prefixes() {
    assert!(command_name_is_claimed_by_builtin("compact"));
    assert!(command_name_is_claimed_by_builtin("comp"));
    assert!(command_name_is_claimed_by_builtin("status"));
    assert!(command_name_is_claimed_by_builtin("stat"));
    assert!(!command_name_is_claimed_by_builtin("review-worktree"));
}

#[test]
fn command_discovery_keeps_extension_usage_and_argument_hint() {
    assert_eq!(
        extension_command_presentation("review-worktree", Some("/review-worktree [focus]".into()),),
        ("/review-worktree [focus]".into(), Some("[focus]".into()),)
    );
    assert_eq!(
        extension_command_presentation("review-worktree", Some("/review-worktrees".into())),
        ("/review-worktree".into(), None),
    );
}

#[test]
fn command_discovery_trims_oversized_resource_metadata() {
    let mut discovery = CommandDiscovery {
        protocol: PROTOCOL_VERSION,
        commands: Vec::new(),
        skills: (0..512)
            .map(|index| SkillSuggestion {
                id: format!("skill-{index}"),
                name: format!("Skill {index}"),
                description: "x".repeat(2_048),
                active: false,
            })
            .collect(),
    };

    assert!(discovery.validate().is_err());
    trim_command_discovery_to_transport_bounds(&mut discovery);
    assert!(discovery.validate().is_ok());
    assert!(discovery.skills.len() < 512);
}

#[tokio::test]
async fn graphical_command_discovery_buffers_stream_events_until_the_actor_resumes() {
    let (commands, mut worker_commands) = mpsc::channel(1);
    let (event_sender, events) = mpsc::channel(1);
    let mut driver = OctetSessionDriver {
        seed: empty_seed(
            SessionId::new("buffered-discovery").unwrap(),
            None,
            ModelSelection {
                provider: "test".into(),
                model: "test".into(),
                reasoning: "off".into(),
            },
            AuthorityProfile::FullAccess,
            1,
        ),
        commands: Some(commands),
        events,
        buffered_events: VecDeque::new(),
        worker: None,
        inspect_only: false,
        inspection: None,
    };
    let first = TimestampedEvent::new(
        1,
        EventPayload::SessionStateChanged {
            state: SessionLiveState::Working,
            active_run_id: None,
        },
    );
    let second = TimestampedEvent::new(
        2,
        EventPayload::SessionStateChanged {
            state: SessionLiveState::Idle,
            active_run_id: None,
        },
    );
    tokio::spawn(async move {
        let WorkerMessage::CommandDiscovery { response } = worker_commands.recv().await.unwrap()
        else {
            panic!("expected command discovery request");
        };
        event_sender.send(first).await.unwrap();
        // This send only completes when command_discovery keeps draining the
        // bounded event stream while it awaits the worker response.
        event_sender.send(second).await.unwrap();
        response
            .send(Ok(CommandDiscovery {
                protocol: PROTOCOL_VERSION,
                commands: Vec::new(),
                skills: Vec::new(),
            }))
            .unwrap();
    });

    let discovery = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        driver.command_discovery(),
    )
    .await
    .expect("command discovery should not be blocked by stream events")
    .unwrap();
    assert_eq!(discovery.protocol, PROTOCOL_VERSION);
    assert!(matches!(
        driver.next_event().await,
        Some(TimestampedEvent {
            payload: EventPayload::SessionStateChanged {
                state: SessionLiveState::Working,
                ..
            },
            ..
        })
    ));
    assert!(matches!(
        driver.next_event().await,
        Some(TimestampedEvent {
            payload: EventPayload::SessionStateChanged {
                state: SessionLiveState::Idle,
                ..
            },
            ..
        })
    ));
}

#[tokio::test]
async fn graphical_command_discovery_keeps_its_event_fifo_bounded() {
    let (commands, mut worker_commands) = mpsc::channel(1);
    let (event_sender, events) = mpsc::channel(1);
    let mut driver = OctetSessionDriver {
        seed: empty_seed(
            SessionId::new("bounded-discovery").unwrap(),
            None,
            ModelSelection {
                provider: "test".into(),
                model: "test".into(),
                reasoning: "off".into(),
            },
            AuthorityProfile::FullAccess,
            1,
        ),
        commands: Some(commands),
        events,
        buffered_events: VecDeque::new(),
        worker: None,
        inspect_only: false,
        inspection: None,
    };
    tokio::spawn(async move {
        let WorkerMessage::CommandDiscovery { response } = worker_commands.recv().await.unwrap()
        else {
            panic!("expected command discovery request");
        };
        for timestamp_ms in 1..=(MAX_BUFFERED_DISCOVERY_EVENTS as u64 + 1) {
            event_sender
                .send(TimestampedEvent::new(
                    timestamp_ms,
                    EventPayload::SessionStateChanged {
                        state: SessionLiveState::Idle,
                        active_run_id: None,
                    },
                ))
                .await
                .unwrap();
        }
        response
            .send(Ok(CommandDiscovery {
                protocol: PROTOCOL_VERSION,
                commands: Vec::new(),
                skills: Vec::new(),
            }))
            .unwrap();
    });

    driver.command_discovery().await.unwrap();
    assert_eq!(driver.buffered_events.len(), MAX_BUFFERED_DISCOVERY_EVENTS);
    for timestamp_ms in 1..=(MAX_BUFFERED_DISCOVERY_EVENTS as u64 + 1) {
        assert_eq!(
            driver.next_event().await.unwrap().timestamp_ms,
            timestamp_ms
        );
    }
}

#[tokio::test]
async fn graphical_command_discovery_uses_tui_order_and_invokes_idle_commands_off_prompt_path() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let skill_dir = workspace.join(".octet/skills/composer-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: Composer Skill\ndescription: Exercise web skill discovery.\n---\n# Composer Skill\n",
    )
    .unwrap();
    let prompt_dir = workspace.join(".octet/prompts");
    std::fs::create_dir_all(&prompt_dir).unwrap();
    std::fs::write(
        prompt_dir.join("composer-prompt.md"),
        "---\ndescription: Exercise web prompt discovery.\nargument-hint: '[focus]'\n---\nReview ${@}.\n",
    )
    .unwrap();
    let (host, session_id, _, _, _) = worker_checkout_fixture(directory.path(), "slash-discovery");
    let mut driver = host.open_session(&session_id).await.unwrap();

    let discovery = driver.command_discovery().await.unwrap();
    assert_eq!(discovery.protocol, PROTOCOL_VERSION);
    let built_ins = commands::slash_commands()
        .iter()
        .map(|command| (command.name, command.usage, command.description))
        .collect::<Vec<_>>();
    let discovered_built_ins = discovery
        .commands
        .iter()
        .take(built_ins.len())
        .map(|command| {
            (
                command.name.as_str(),
                command.usage.as_str(),
                command.description.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(discovered_built_ins, built_ins);
    assert!(discovery.commands.iter().any(|command| {
        command.name == "composer-prompt"
            && command.argument_hint.as_deref() == Some("[focus]")
            && command.kind == CommandSuggestionKind::Prompt
    }));
    assert!(discovery.skills.iter().any(|skill| {
        skill.id == "composer-skill" && skill.name == "Composer Skill" && !skill.active
    }));
    discovery.validate().unwrap();

    let renamed = driver
        .dispatch(SessionCommand::InvokeSlashCommand {
            invocation: SlashCommandInvocation {
                invocation: "/name Composer discovery".into(),
            },
        })
        .await
        .unwrap();
    assert!(matches!(
        renamed.events.as_slice(),
        [TimestampedEvent {
            payload: EventPayload::SessionMetadataChanged {
                title: Some(title),
                pinned: None,
                archived: None,
            },
            ..
        }] if title == "Composer discovery"
    ));

    let skills = driver
        .dispatch(SessionCommand::InvokeSlashCommand {
            invocation: SlashCommandInvocation {
                invocation: "/skills active".into(),
            },
        })
        .await
        .unwrap();
    assert!(skills.events.is_empty());
    assert!(skills.run_id.is_none());

    let loaded = driver
        .dispatch(SessionCommand::InvokeSlashCommand {
            invocation: SlashCommandInvocation {
                invocation: "/skills load composer-skill".into(),
            },
        })
        .await
        .unwrap();
    assert!(!loaded.events.is_empty());
    assert!(driver
        .command_discovery()
        .await
        .unwrap()
        .skills
        .iter()
        .any(|skill| skill.id == "composer-skill" && skill.active));

    let unloaded = driver
        .dispatch(SessionCommand::InvokeSlashCommand {
            invocation: SlashCommandInvocation {
                invocation: "/skills off composer-skill".into(),
            },
        })
        .await
        .unwrap();
    assert!(!unloaded.events.is_empty());
    assert!(driver
        .command_discovery()
        .await
        .unwrap()
        .skills
        .iter()
        .any(|skill| skill.id == "composer-skill" && !skill.active));

    assert_eq!(
        driver
            .dispatch(SessionCommand::InvokeSlashCommand {
                invocation: SlashCommandInvocation {
                    invocation: "/not-a-command".into(),
                },
            })
            .await
            .unwrap_err(),
        ServiceError::InvalidBoundary
    );
    driver.shutdown().await;
}

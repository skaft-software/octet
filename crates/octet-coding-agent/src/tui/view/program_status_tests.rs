use super::*;
use crate::tui::view::{InteractiveShell, OrdinarySurfaceMetadata};
use octet_agent::{AgentEvent, EntryId, FinishReason};

#[test]
fn host_runs_are_working_until_authoritative_success_failure_or_cancellation() {
    let mut shell = InteractiveShell::test_shell();
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Idle);
    for (reason, expected) in [
        (FinishReason::Completed, ProgramStatus::Done),
        (
            FinishReason::Failed(octet_agent::AgentError::ProviderContextPreparation(
                "private failure details",
            )),
            ProgramStatus::Error,
        ),
        (FinishReason::Aborted, ProgramStatus::Idle),
        (FinishReason::MaxTurns, ProgramStatus::Error),
    ] {
        let run = shell.begin_run("fixture");
        assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
        shell.set_awaiting_provider(run);
        assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
        shell.on_run_event(
            run,
            &AgentEvent::RunFinished {
                head: EntryId("fixture".into()),
                reason,
            },
        );
        assert_eq!(activity(&shell.state.borrow()), expected);
    }
}

#[test]
fn only_actual_gates_block_not_ordinary_reports_or_private_text() {
    let mut shell = InteractiveShell::test_shell();
    let run = shell.begin_run("fixture");
    shell.open_panel(Panel::ReadOnlyDocument {
        title: "Not a blocking question".into(),
        text: "Private report".into(),
        styled: false,
        scroll_from_bottom: 0,
    });
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("private approval arguments"),
        items: vec!["Deny".into(), "Approve".into()],
        descriptions: vec![],
        selected: 0,
        filter: String::new(),
        action: PanelAction::Confirmation,
    });
    assert_eq!(
        activity(&shell.state.borrow()),
        ProgramStatus::Blocked(BlockedKind::Permission)
    );
    shell.set_terminal_permission_pending(true);
    shell.open_panel(Panel::ReadOnlyDocument {
        title: "Private technical details".into(),
        text: "not exported".into(),
        styled: false,
        scroll_from_bottom: 0,
    });
    assert_eq!(
        activity(&shell.state.borrow()),
        ProgramStatus::Blocked(BlockedKind::Permission)
    );
    shell.set_terminal_permission_pending(false);
    shell.close_panel();
    for secret in [false, true] {
        shell.begin_tool_input("private prompt\x1b\x07\u{85}", secret);
        let status = activity(&shell.state.borrow());
        assert_eq!(status, ProgramStatus::Blocked(BlockedKind::Question));
        assert_eq!(
            status.sequence("octet").unwrap(),
            "\x1b]7501;state=blocked:kind=question:app=octet\x1b\\"
        );
        shell.end_tool_input();
        assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
    }
    shell.interrupt_run(run);
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Idle);
}

#[test]
fn auth_requires_a_real_auth_owner_and_exchanging_is_not_blocked() {
    use crate::auth::codex::{DeviceLoginPhase, LoginProgress};
    let mut shell = InteractiveShell::test_shell();
    shell.set_terminal_auth_progress(&LoginProgress::Starting);
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
    for phase in [
        DeviceLoginPhase::Waiting,
        DeviceLoginPhase::SlowDown,
        DeviceLoginPhase::Exchanging,
    ] {
        shell.set_terminal_auth_progress(&LoginProgress::Device {
            user_code: "NEVER-IN-TAB".into(),
            phase,
        });
        let status = activity(&shell.state.borrow());
        assert_eq!(
            status,
            if phase == DeviceLoginPhase::Exchanging {
                ProgramStatus::Working
            } else {
                ProgramStatus::Blocked(BlockedKind::Auth)
            }
        );
        assert!(!status.sequence("octet").unwrap().contains("NEVER-IN-TAB"));
    }
    shell.set_terminal_auth_progress(&LoginProgress::Exchanging);
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
    shell.set_terminal_auth_progress(&LoginProgress::Saving);
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
    for (outcome, expected) in [
        (Ok(Some(())), ProgramStatus::Done),
        (Ok(None), ProgramStatus::Idle),
        (
            Err(anyhow::anyhow!("secret error details")),
            ProgramStatus::Error,
        ),
    ] {
        shell.finish_terminal_auth(&outcome);
        assert_eq!(activity(&shell.state.borrow()), expected);
    }
    shell.begin_run("fixture");
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
}

#[test]
fn publisher_dedupes_hands_back_transient_states_and_never_clears_results() {
    for status in [
        ProgramStatus::Working,
        ProgramStatus::Blocked(BlockedKind::Auth),
        ProgramStatus::Blocked(BlockedKind::Permission),
        ProgramStatus::Blocked(BlockedKind::Question),
    ] {
        let mut publisher = Publisher::default();
        let mut writes = Vec::new();
        publisher.sync(status, None, |s| writes.push(s));
        publisher.sync(status, None, |s| writes.push(s));
        publisher.handoff(|s| writes.push(s));
        publisher.handoff(|s| writes.push(s));
        publisher.sync(status, None, |s| writes.push(s));
        assert_eq!(writes, vec![status, ProgramStatus::Idle, status]);
    }
    for status in [
        ProgramStatus::Done,
        ProgramStatus::Error,
        ProgramStatus::Idle,
    ] {
        let mut publisher = Publisher::default();
        let mut writes = Vec::new();
        publisher.sync(status, None, |s| writes.push(s));
        publisher.handoff(|s| writes.push(s));
        publisher.sync(status, None, |s| writes.push(s));
        assert_eq!(writes, vec![status]);
    }
}

#[test]
fn coalesced_distinct_runs_republish_their_result_without_repainting_old_results() {
    let mut shell = InteractiveShell::test_shell();
    let mut publisher = Publisher::default();
    let mut writes = Vec::new();
    for _ in 0..2 {
        let id = shell.begin_run("fixture");
        shell.on_run_event(
            id,
            &AgentEvent::RunFinished {
                head: EntryId("fixture".into()),
                reason: FinishReason::Completed,
            },
        );
        let state = shell.state.borrow();
        publisher.sync(snapshot(&state).0, snapshot(&state).1, |s| writes.push(s));
        publisher.sync(snapshot(&state).0, snapshot(&state).1, |s| writes.push(s));
    }
    assert_eq!(writes, [ProgramStatus::Done, ProgramStatus::Done]);
}

#[test]
fn coalesced_foreground_results_have_distinct_owners_and_preserve_parent_runs() {
    for (exit, reason, expected) in [
        (0, FinishReason::Completed, ProgramStatus::Done),
        (1, FinishReason::MaxTurns, ProgramStatus::Error),
    ] {
        let mut shell = InteractiveShell::test_shell();
        let run = shell.begin_run("fixture");
        shell.on_run_event(
            run,
            &AgentEvent::RunFinished {
                head: EntryId("fixture".into()),
                reason,
            },
        );
        let mut publisher = Publisher::default();
        let mut writes = Vec::new();
        {
            let state = shell.state.borrow();
            publisher.sync(snapshot(&state).0, snapshot(&state).1, |s| writes.push(s));
        }
        for _ in 0..2 {
            let id = shell.append_shell_in_progress("inert command".into());
            shell.finalize_shell(&id, String::new(), exit);
            let state = shell.state.borrow();
            publisher.sync(snapshot(&state).0, snapshot(&state).1, |s| writes.push(s));
            publisher.handoff(|s| writes.push(s));
            publisher.sync(snapshot(&state).0, snapshot(&state).1, |s| writes.push(s));
        }
        assert_eq!(writes, [expected, expected, expected]);
        shell.begin_run("parent");
        let id = shell.append_shell_in_progress("inert nested command".into());
        shell.finalize_shell(&id, String::new(), exit);
        assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
    }
}

#[test]
fn coalesced_auth_failures_have_distinct_owners_without_progress_reassertions() {
    let mut shell = InteractiveShell::test_shell();
    let mut publisher = Publisher::default();
    let mut writes = Vec::new();
    for _ in 0..2 {
        shell.begin_terminal_operation();
        shell.set_terminal_auth_progress(&crate::auth::codex::LoginProgress::Starting);
        shell.set_terminal_auth_progress(&crate::auth::codex::LoginProgress::Exchanging);
        shell.finish_terminal_auth(&Err(anyhow::anyhow!("inert auth failure")));
        let state = shell.state.borrow();
        let (status, owner) = snapshot(&state);
        publisher.sync(status, owner, |s| writes.push(s));
        publisher.handoff(|s| writes.push(s));
        publisher.sync(status, owner, |s| writes.push(s));
    }
    assert_eq!(writes, [ProgramStatus::Error, ProgramStatus::Error]);
}

#[test]
fn local_shell_settlement_cannot_inherit_an_old_auth_or_agent_result() {
    let mut shell = InteractiveShell::test_shell();
    shell.finish_terminal_auth(&Err(anyhow::anyhow!("inert auth failure")));
    for exit in [0, 1, -1] {
        let id = shell.append_shell_in_progress("inert command".into());
        assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Working);
        shell.finalize_shell(&id, "private command output".into(), exit);
        assert_eq!(
            activity(&shell.state.borrow()),
            if exit == 0 {
                ProgramStatus::Done
            } else {
                ProgramStatus::Error
            }
        );
    }
    shell.cancel_terminal_operation();
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Idle);
}

#[test]
fn login_catalog_reload_labels_keep_one_working_foreground_until_validation() {
    use crate::auth::codex::LoginProgress;
    for previous in [None, Some(true), Some(false)] {
        for success in [true, false] {
            let mut shell = InteractiveShell::test_shell();
            if let Some(completed) = previous {
                let id = shell.begin_run("fixture");
                shell.on_run_event(
                    id,
                    &AgentEvent::RunFinished {
                        head: EntryId("fixture".into()),
                        reason: if completed {
                            FinishReason::Completed
                        } else {
                            FinishReason::MaxTurns
                        },
                    },
                );
            }
            let mut publisher = Publisher::default();
            let mut writes = Vec::new();
            let mut sample = |shell: &InteractiveShell, writes: &mut Vec<ProgramStatus>| {
                let state = shell.state.borrow();
                let (status, owner) = snapshot(&state);
                publisher.sync(status, owner, |s| writes.push(s));
                (status, owner)
            };
            let (seed, _) = sample(&shell, &mut writes);
            shell.set_run_label("signing in to ChatGPT…");
            shell.begin_terminal_operation();
            shell.set_terminal_auth_progress(&LoginProgress::Starting);
            let generation = shell.state.borrow().terminal_operation_generation;
            let foreground = Some(ActivityOwner::Foreground(generation));
            assert_eq!(
                sample(&shell, &mut writes),
                (ProgramStatus::Working, foreground)
            );
            shell.set_run_label("idle");
            shell.set_terminal_auth_progress(&LoginProgress::SignedIn);
            assert_eq!(
                sample(&shell, &mut writes),
                (ProgramStatus::Working, foreground)
            );
            shell.set_run_label("reloading models…");
            assert_eq!(
                sample(&shell, &mut writes),
                (ProgramStatus::Working, foreground)
            );
            shell.set_run_label("idle");
            assert_eq!(
                sample(&shell, &mut writes),
                (ProgramStatus::Working, foreground)
            );
            shell.finish_terminal_operation(success);
            let settled = if success {
                ProgramStatus::Done
            } else {
                ProgramStatus::Error
            };
            assert_eq!(sample(&shell, &mut writes), (settled, foreground));
            assert_eq!(sample(&shell, &mut writes), (settled, foreground));
            assert_eq!(writes, [seed, ProgramStatus::Working, settled]);
        }
    }
}

#[test]
fn generic_labels_clear_only_settled_foreground_results() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_terminal_operation();
    shell.finish_terminal_operation(false);
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Error);
    shell.set_run_label("reloading models…");
    let state = shell.state.borrow();
    assert_eq!(snapshot(&state), (ProgramStatus::Working, None));
    drop(state);
    shell.set_run_label("idle");
    assert_eq!(snapshot(&shell.state.borrow()), (ProgramStatus::Idle, None));
    shell.begin_terminal_operation();
    shell.set_run_label("reloading models…");
    shell.cancel_terminal_operation();
    assert_eq!(activity(&shell.state.borrow()), ProgramStatus::Idle);
    shell.set_run_label("idle");
    assert_eq!(snapshot(&shell.state.borrow()), (ProgramStatus::Idle, None));
}

#[test]
fn foreground_reload_results_never_settle_an_active_parent_run() {
    for success in [true, false] {
        let mut shell = InteractiveShell::test_shell();
        let run = shell.begin_run("parent");
        shell.begin_terminal_operation();
        shell.set_run_label("reloading models…");
        shell.set_run_label("idle");
        shell.finish_terminal_operation(success);
        let state = shell.state.borrow();
        assert_eq!(
            snapshot(&state),
            (ProgramStatus::Working, Some(ActivityOwner::Run(run)))
        );
    }
}

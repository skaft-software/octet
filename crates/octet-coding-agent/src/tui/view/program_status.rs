//! Payload-free terminal chrome, projected only from host-owned lifecycle facts.
//!
//! Session names still belong exclusively to OSC 2. Never copy prompts, tool
//! arguments, approval details, OAuth instructions/codes or errors into OSC 7501.

use sexy_tui_rs::program_status::{BlockedKind, ProgramStatus};

use super::{Panel, PanelAction, ShellState, TranscriptBlock};
use crate::presentation::{RunId, RunOutcome, RunPhase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivityOwner {
    Run(RunId),
    Foreground(u64),
}

pub(super) fn snapshot(state: &ShellState) -> (ProgramStatus, Option<ActivityOwner>) {
    // A foreground result cannot settle an active parent. Its owner is used
    // only while it is the authoritative source of the reported activity.
    let foreground = state
        .terminal_operation_status
        .filter(|status| status.is_transient() || !state.run.is_active());
    let owner = if foreground.is_some() {
        Some(ActivityOwner::Foreground(
            state.terminal_operation_generation,
        ))
    } else {
        state.run.current_id().map(ActivityOwner::Run)
    };
    (activity_with_foreground(state, foreground), owner)
}

#[cfg(test)]
fn activity(state: &ShellState) -> ProgramStatus {
    snapshot(state).0
}

fn activity_with_foreground(
    state: &ShellState,
    foreground: Option<ProgramStatus>,
) -> ProgramStatus {
    if state.terminal_permission_pending
        || matches!(
            state.panel,
            Some(Panel::SelectList {
                action: PanelAction::Confirmation,
                ..
            })
        )
    {
        return ProgramStatus::Blocked(BlockedKind::Permission);
    }
    if state.tool_input_prompt.is_some() {
        // Secret is not synonymous with authentication. A private tool question
        // is still a question; only a real auth owner reports `kind=auth`.
        return ProgramStatus::Blocked(BlockedKind::Question);
    }
    if let Some(status) = foreground {
        return status;
    }
    if !state.run_label.is_empty()
        || state.active_event_blocks.iter().any(|index| {
            matches!(state.transcript.get(*index), Some(TranscriptBlock::Shell(shell)) if shell.running)
        })
    {
        return ProgramStatus::Working;
    }
    match state.run.current().map(|run| run.phase()) {
        Some(RunPhase::AwaitingApproval { .. }) => ProgramStatus::Blocked(BlockedKind::Permission),
        Some(RunPhase::Finished(outcome)) => match outcome {
            RunOutcome::Completed { .. } | RunOutcome::CompletedWithWarnings { .. } => {
                ProgramStatus::Done
            }
            RunOutcome::Failed { .. } => ProgramStatus::Error,
            RunOutcome::Interrupted { .. } => ProgramStatus::Idle,
            RunOutcome::NeedsInput { .. } => ProgramStatus::Blocked(BlockedKind::Question),
        },
        Some(_) => ProgramStatus::Working,
        None => ProgramStatus::Idle,
    }
}

/// Shared by replacement renderers, not by sessions on other ptys. The terminal
/// owns done/error lifetime; do not reassert them on every paint or clear them
/// when changing screens, suspending, or leaving the UI.
#[derive(Default)]
pub(super) struct Publisher {
    last: Option<ProgramStatus>,
    owner: Option<ActivityOwner>,
}

impl Publisher {
    pub(super) fn sync(
        &mut self,
        status: ProgramStatus,
        owner: Option<ActivityOwner>,
        mut write: impl FnMut(ProgramStatus),
    ) {
        if self.last != Some(status) || self.owner != owner {
            write(status);
            self.last = Some(status);
            self.owner = owner;
        }
    }

    pub(super) fn handoff(&mut self, write: impl FnMut(ProgramStatus)) {
        if self.last.is_some_and(ProgramStatus::is_transient) {
            self.sync(ProgramStatus::Idle, self.owner, write);
        }
    }
}

impl super::InteractiveShell {
    pub(crate) fn begin_terminal_operation(&mut self) {
        let mut state = self.state.borrow_mut();
        state.terminal_operation_generation += 1;
        state.terminal_operation_status = Some(ProgramStatus::Working);
    }

    #[cfg(test)]
    pub(crate) fn test_terminal_activity(&self) -> ProgramStatus {
        activity(&self.state.borrow())
    }

    pub(crate) fn cancel_terminal_operation(&mut self) {
        self.state.borrow_mut().terminal_operation_status = Some(ProgramStatus::Idle);
    }

    pub(crate) fn set_terminal_permission_pending(&mut self, pending: bool) {
        self.state.borrow_mut().terminal_permission_pending = pending;
    }

    pub(crate) fn set_terminal_auth_progress(
        &mut self,
        progress: &crate::auth::codex::LoginProgress,
    ) {
        use crate::auth::codex::{DeviceLoginPhase, LoginProgress};
        self.state.borrow_mut().terminal_operation_status = Some(match progress {
            LoginProgress::Browser { .. }
            | LoginProgress::Device {
                phase: DeviceLoginPhase::Waiting | DeviceLoginPhase::SlowDown,
                ..
            } => ProgramStatus::Blocked(BlockedKind::Auth),
            // Credential commit is not completion of the catalog reload.
            LoginProgress::SignedIn => ProgramStatus::Working,
            _ => ProgramStatus::Working,
        });
    }

    pub(crate) fn finish_terminal_operation(&mut self, success: bool) {
        self.state.borrow_mut().terminal_operation_status = Some(if success {
            ProgramStatus::Done
        } else {
            ProgramStatus::Error
        });
    }

    pub(crate) fn finish_terminal_auth(&mut self, outcome: &anyhow::Result<Option<()>>) {
        self.state.borrow_mut().terminal_operation_status = Some(match outcome {
            Ok(Some(())) => ProgramStatus::Done,
            Ok(None) => ProgramStatus::Idle,
            Err(_) => ProgramStatus::Error,
        });
    }
}

#[cfg(test)]
#[path = "program_status_tests.rs"]
mod tests;

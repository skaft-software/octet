//! Queued tasks, permits, worker outcomes and waiter guards.

use super::*;

pub(super) enum PermitWait {
    Acquired(OwnedSemaphorePermit),
    TimedOut,
    Interrupted,
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct QueuedFollowUp {
    /// Cryptographically random host delivery identity, persisted until the
    /// child session records this exact envelope.
    #[serde(default)]
    pub(super) delivery_id: String,
    pub(super) from: String,
    pub(super) message: String,
    /// Number of failed attempts to append this input to the child session.
    #[serde(default)]
    pub(super) attempts: u8,
}

impl QueuedFollowUp {
    pub(super) fn usage(&self) -> QueueUsage {
        QueueUsage {
            messages: 1,
            bytes: self.from.len().saturating_add(self.message.len()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct QueuedInitialTask {
    pub(super) task: String,
    pub(super) delivery_id: String,
    pub(super) attempts: u8,
}

#[derive(Clone)]
pub(super) enum QueuedTask {
    Initial(QueuedInitialTask),
    FollowUp(QueuedFollowUp),
}

impl QueuedTask {
    #[cfg(test)]
    pub(super) fn initial(task: String) -> Self {
        Self::Initial(QueuedInitialTask {
            task,
            delivery_id: new_delivery_id().unwrap(),
            attempts: 0,
        })
    }

    pub(super) fn delivery_id(&self) -> &str {
        match self {
            Self::Initial(task) => &task.delivery_id,
            Self::FollowUp(task) => &task.delivery_id,
        }
    }

    pub(super) fn follow_up(follow_up: QueuedFollowUp) -> Self {
        Self::FollowUp(follow_up)
    }

    pub(super) fn format(&self, pending: &[DirectedMessage]) -> String {
        match self {
            Self::Initial(task) => format_initial_task(
                &format!(
                    "<octet_delegation_delivery id=\"{}\" kind=\"initial\">\n{}\n</octet_delegation_delivery>",
                    task.delivery_id, task.task
                ),
                pending,
            ),
            Self::FollowUp(follow_up) => format_follow_up(follow_up, pending),
        }
    }

    pub(super) fn attempts(&self) -> u8 {
        match self {
            Self::Initial(task) => task.attempts,
            Self::FollowUp(follow_up) => follow_up.attempts,
        }
    }

    pub(super) fn increment_attempts(&mut self) {
        match self {
            Self::Initial(task) => task.attempts = task.attempts.saturating_add(1),
            Self::FollowUp(follow_up) => follow_up.attempts = follow_up.attempts.saturating_add(1),
        }
    }
}

pub(super) enum TaskRestore {
    NotRestored,
    Restored { attempts: u8 },
    DeadLettered { attempts: u8 },
}

pub(super) fn restore_undelivered_task(
    queued_tasks: &mut VecDeque<QueuedTask>,
    mut task: QueuedTask,
    task_delivered: bool,
    outcome: &WorkerOutcome,
) -> TaskRestore {
    let should_restore = !task_delivered
        && match outcome {
            WorkerOutcome::Shutdown => false,
            WorkerOutcome::Interrupted | WorkerOutcome::TimedOut => {
                matches!(&task, QueuedTask::FollowUp(_))
            }
            WorkerOutcome::LimitReached { .. }
            | WorkerOutcome::Completed(_)
            | WorkerOutcome::Failed(_) => true,
        };
    if !should_restore {
        return TaskRestore::NotRestored;
    }
    task.increment_attempts();
    if task.attempts() >= MAX_UNDELIVERED_TASK_ATTEMPTS {
        return TaskRestore::DeadLettered {
            attempts: task.attempts(),
        };
    }
    let attempts = task.attempts();
    queued_tasks.push_front(task);
    TaskRestore::Restored { attempts }
}

pub(super) struct WorkerExecution {
    pub(super) outcome: WorkerOutcome,
    pub(super) deferred_follow_ups: VecDeque<QueuedFollowUp>,
    pub(super) acknowledged_follow_ups: QueueUsage,
    pub(super) task_delivered: bool,
}

impl WorkerExecution {
    pub(super) fn new(outcome: WorkerOutcome) -> Self {
        Self {
            outcome,
            deferred_follow_ups: VecDeque::new(),
            acknowledged_follow_ups: QueueUsage::default(),
            task_delivered: false,
        }
    }
}

#[derive(Debug)]
pub(super) enum WorkerOutcome {
    Completed(String),
    /// The run exhausted its configured per-run turn budget after producing
    /// the bounded output collected so far.
    LimitReached {
        output: String,
        turn_count: u64,
        turn_limit: u64,
    },
    Interrupted,
    TimedOut,
    Failed(String),
    Shutdown,
}

pub(super) struct WaiterGuard<'a> {
    pub(super) manager: &'a DelegationManager,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_waiters = state.active_waiters.saturating_sub(1);
    }
}

//! Summarization retry: one shared retry policy for every summarization call,
//! and typed outcomes that keep a retry distinguishable from a failure.
//!
//! Pi shares a single retry policy between compaction and branch-summary
//! summarization calls (`_summarizationRetryCallbacks`,
//! `packages/coding-agent/src/core/agent-session.ts`, and
//! `retryAssistantCall`, `packages/ai/src/utils/retry.ts`), so a single
//! transient stream drop no longer fails a whole compaction. Three rules matter
//! and are implemented here:
//!
//! 1. **A retry is not a failure.** A scheduled retry is reported through its own
//!    typed outcome and its own `summarization_retry_scheduled` diagnostic; the
//!    compaction boundary is still live and must not publish a failure or close
//!    its bracket. Only an exhausted, non-retryable, or aborted sequence becomes
//!    a [`CompactionFailure`], whose diagnostic names the summarization cause
//!    explicitly instead of reusing the retry wording.
//! 2. **Aborts and deterministic errors never retry.**
//! 3. **A retry never duplicates durable state.** The summarized result is
//!    handed to the caller's commit exactly once, and only when an attempt
//!    succeeded, so the number of durable summary records is `0` or `1` no
//!    matter how many provider attempts ran.
//!
//! The live agent's auxiliary recovery consumer uses this bounded policy for
//! ordinary-route compaction and branch summaries, preserving cancellation,
//! durable unknown-usage records and hard ceilings. Qualified Codex operations
//! retain their existing recovery envelope instead of stacking retry loops.
//! `Agent::summarize_with_retry` and `Agent::summarize_branch_with_retry` expose
//! that same consumer to hosts; successful text is committed once by the caller.

use std::time::Duration;

/// Default total attempts for one summarization call, including the first.
pub const DEFAULT_SUMMARIZATION_MAX_ATTEMPTS: usize = 3;
/// Hard cap for one summarization call's attempts.
pub const MAX_SUMMARIZATION_ATTEMPTS: usize = 8;
/// Delay before the first summarization retry; later delays double.
pub const DEFAULT_SUMMARIZATION_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Hard cap for one summarization retry delay, matching Pi's `maxAgentDelayMs`.
pub const MAX_SUMMARIZATION_BACKOFF: Duration = Duration::from_secs(60);

/// Bounded retry behavior for one summarization call.
///
/// `max_attempts` includes the initial request and is clamped to
/// [`MAX_SUMMARIZATION_ATTEMPTS`]; a zero value is treated as one attempt. Either
/// backoff may be zero, which disables the delay without disabling retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SummarizationRetryPolicy {
    /// Total attempts allowed, including the first.
    pub max_attempts: usize,
    /// Delay before the first retry; later delays double up to `max_backoff`.
    pub initial_backoff: Duration,
    /// Maximum delay between summarization attempts.
    pub max_backoff: Duration,
}

impl Default for SummarizationRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_SUMMARIZATION_MAX_ATTEMPTS,
            initial_backoff: DEFAULT_SUMMARIZATION_INITIAL_BACKOFF,
            max_backoff: MAX_SUMMARIZATION_BACKOFF,
        }
    }
}

impl SummarizationRetryPolicy {
    /// A policy that never retries.
    pub fn disabled() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Normalized attempt count for one call.
    pub fn attempts(&self) -> usize {
        self.max_attempts.clamp(1, MAX_SUMMARIZATION_ATTEMPTS)
    }

    /// Deterministic capped exponential delay for a retry numbered from one,
    /// mirroring Pi's `retryDelayMs`: `base * 2^(attempt-1)` capped by the
    /// policy and by [`MAX_SUMMARIZATION_BACKOFF`].
    pub fn backoff_for_retry(&self, retry_number: usize) -> Duration {
        let cap = self.max_backoff.min(MAX_SUMMARIZATION_BACKOFF);
        if retry_number == 0 || self.initial_backoff.is_zero() || cap.is_zero() {
            return Duration::ZERO;
        }
        let mut delay = self.initial_backoff.min(cap);
        for _ in 1..retry_number.min(64) {
            delay = delay.saturating_mul(2);
        }
        delay.min(cap)
    }
}

/// Outcome of one summarization attempt, as the provider layer reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummarizationAttempt {
    /// A complete summary was produced.
    Succeeded {
        /// Size of the produced summary in bytes.
        summary_bytes: usize,
    },
    /// A transient provider or transport failure the policy may retry.
    RetryableFailure {
        /// Provider error text.
        message: String,
    },
    /// A deterministic failure the policy must not retry (for example a
    /// non-retryable provider error or a summary that called a tool).
    NonRetryableFailure {
        /// Deterministic error text.
        message: String,
    },
    /// The request was aborted.
    Aborted,
}

impl SummarizationAttempt {
    /// Whether the policy may retry this attempt.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RetryableFailure { .. })
    }
}

/// Diagnostics emitted around each summarization attempt. These are the octet
/// spelling of Pi's `summarization_retry_scheduled` / `_attempt_start` /
/// `_finished` callbacks and carry no summary content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummarizationDiagnostic {
    /// A retry was scheduled after attempt `attempt` failed transiently.
    RetryScheduled {
        /// Attempt that failed (1-indexed).
        attempt: usize,
        /// Total attempts allowed.
        max_attempts: usize,
        /// Backoff delay before the next attempt.
        delay_ms: u64,
        /// Error text of the failed attempt.
        error: String,
    },
    /// A retried attempt is starting.
    AttemptStart {
        /// Attempt number starting now.
        attempt: usize,
    },
    /// The summarization call finished, successfully or not.
    Finished {
        /// Whether a summary was produced.
        succeeded: bool,
        /// Attempts performed.
        attempts: usize,
        /// Final error text, when unsuccessful.
        error: Option<String>,
    },
}

/// Why a summarization call failed terminally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummarizationFailureKind {
    /// The provider reported a deterministic failure.
    NonRetryable,
    /// Every allowed attempt failed transiently.
    RetriesExhausted,
    /// The request was aborted.
    Aborted,
}

/// A terminal summarization failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummarizationFailure {
    /// Classified cause.
    pub kind: SummarizationFailureKind,
    /// Attempts performed, including the first.
    pub attempts: usize,
    /// Error text of the final attempt.
    pub message: String,
}

impl SummarizationFailure {
    /// Diagnostic text. Never shares wording with a scheduled retry, so a host
    /// cannot render a retry as a failure or vice versa.
    pub fn diagnostic(&self) -> String {
        match self.kind {
            SummarizationFailureKind::NonRetryable => format!(
                "summarization attempt {} failed deterministically: {}",
                self.attempts, self.message
            ),
            SummarizationFailureKind::RetriesExhausted => format!(
                "summarization retries exhausted after {} attempts: {}",
                self.attempts, self.message
            ),
            SummarizationFailureKind::Aborted => format!(
                "summarization was aborted after {} attempt(s)",
                self.attempts
            ),
        }
    }
}

/// Final outcome of one summarization call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummarizationOutcome {
    /// A summary was produced on attempt `attempts`.
    Succeeded {
        /// Attempts performed, including the first.
        attempts: usize,
        /// Size of the produced summary in bytes.
        summary_bytes: usize,
    },
    /// No summary exists.
    Failed(SummarizationFailure),
}

/// A retry the compaction boundary scheduled.
///
/// This is deliberately **not** a [`CompactionFailure`]: the boundary is still
/// live, must not publish a failure, and must not close its bracket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummarizationRetryScheduled {
    /// Attempt that failed (1-indexed).
    pub attempt: usize,
    /// Total attempts allowed.
    pub max_attempts: usize,
    /// Backoff delay before the next attempt.
    pub delay: Duration,
    /// Error text of the failed attempt.
    pub error: String,
}

impl SummarizationRetryScheduled {
    /// Diagnostic text for the retry. Distinct from every failure diagnostic.
    pub fn diagnostic(&self) -> String {
        format!(
            "summarization retry scheduled {}/{} in {}ms: {}",
            self.attempt,
            self.max_attempts,
            self.delay.as_millis(),
            self.error
        )
    }
}

/// Why a compaction boundary failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionFailureKind {
    /// The summarization call failed terminally; the cause is preserved.
    Summarization(SummarizationFailureKind),
    /// The summary existed but could not be durably recorded.
    DurableWrite,
}

/// A compaction boundary failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionFailure {
    /// Classified cause.
    pub kind: CompactionFailureKind,
    /// Summarization attempts performed.
    pub attempts: usize,
    /// Diagnostic text; always names the compaction boundary so it cannot be
    /// mistaken for a scheduled retry.
    pub diagnostic: String,
}

impl CompactionFailure {
    /// Builds the boundary failure caused by a terminal summarization failure.
    pub fn from_summarization(failure: &SummarizationFailure) -> Self {
        Self {
            kind: CompactionFailureKind::Summarization(failure.kind),
            attempts: failure.attempts,
            diagnostic: format!("compaction boundary failed: {}", failure.diagnostic()),
        }
    }

    /// Builds the boundary failure caused by a durable write that did not land.
    pub fn durable_write(attempts: usize, error: &str) -> Self {
        Self {
            kind: CompactionFailureKind::DurableWrite,
            attempts,
            diagnostic: format!(
                "compaction boundary failed: durable summary write failed: {error}"
            ),
        }
    }
}

/// What the compaction boundary should do after a summarization step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactionStepOutcome {
    /// A retry is scheduled; the boundary is **not** failed.
    Retrying(SummarizationRetryScheduled),
    /// The boundary committed its summary exactly once.
    Committed {
        /// Summarization attempts performed.
        attempts: usize,
        /// Durable summary records written; always exactly one.
        durable_summary_records: u32,
    },
    /// The boundary failed.
    Failed(CompactionFailure),
}

impl CompactionStepOutcome {
    /// Classifies one summarization diagnostic from the boundary's point of view.
    ///
    /// Returns `None` for diagnostics that do not themselves change the boundary
    /// state (`AttemptStart`).
    pub fn from_diagnostic(diagnostic: &SummarizationDiagnostic) -> Option<Self> {
        match diagnostic {
            SummarizationDiagnostic::RetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                error,
            } => Some(Self::Retrying(SummarizationRetryScheduled {
                attempt: *attempt,
                max_attempts: *max_attempts,
                delay: Duration::from_millis(*delay_ms),
                error: error.clone(),
            })),
            SummarizationDiagnostic::AttemptStart { .. } => None,
            SummarizationDiagnostic::Finished { .. } => None,
        }
    }

    /// Whether this outcome means the compaction boundary failed.
    pub fn is_compaction_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// Everything observed while retrying one summarization call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummarizationRun {
    /// Final outcome of the summarization call.
    pub outcome: SummarizationOutcome,
    /// Diagnostics in emission order.
    pub diagnostics: Vec<SummarizationDiagnostic>,
    /// Error from the durable commit, when a successful summary could not be
    /// recorded.
    pub commit_error: Option<String>,
}

impl SummarizationRun {
    /// Durable summary records this run produced: exactly one on success, zero
    /// otherwise. A retry can never increase this number.
    pub fn durable_summary_records(&self) -> u32 {
        match (&self.outcome, &self.commit_error) {
            (SummarizationOutcome::Succeeded { .. }, None) => 1,
            _ => 0,
        }
    }

    /// The compaction boundary's view of this run.
    pub fn compaction_outcome(&self) -> CompactionStepOutcome {
        match &self.outcome {
            SummarizationOutcome::Succeeded { attempts, .. } => match &self.commit_error {
                None => CompactionStepOutcome::Committed {
                    attempts: *attempts,
                    durable_summary_records: 1,
                },
                Some(error) => CompactionStepOutcome::Failed(CompactionFailure::durable_write(
                    *attempts, error,
                )),
            },
            SummarizationOutcome::Failed(failure) => {
                CompactionStepOutcome::Failed(CompactionFailure::from_summarization(failure))
            }
        }
    }
}

/// Runs one summarization call under `policy`, committing a produced summary at
/// most once.
///
/// `attempt` is called with the 1-indexed attempt number and returns that
/// attempt's outcome. `commit` is called **only** after a successful attempt, so
/// retries cannot duplicate durable state; if it fails, the run reports
/// [`CompactionFailureKind::DurableWrite`] rather than a summarization failure,
/// because the summary itself succeeded.
pub async fn run_summarization_with_retry<F, Fut, C>(
    policy: &SummarizationRetryPolicy,
    mut attempt: F,
    commit: C,
) -> SummarizationRun
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = SummarizationAttempt>,
    C: FnOnce(usize) -> Result<(), String>,
{
    let max_attempts = policy.attempts();
    let mut diagnostics = Vec::new();
    let mut number = 1usize;
    loop {
        let outcome = attempt(number).await;
        match outcome {
            SummarizationAttempt::Succeeded { summary_bytes } => {
                let commit_result = commit(number);
                diagnostics.push(SummarizationDiagnostic::Finished {
                    succeeded: true,
                    attempts: number,
                    error: None,
                });
                return SummarizationRun {
                    outcome: SummarizationOutcome::Succeeded {
                        attempts: number,
                        summary_bytes,
                    },
                    diagnostics,
                    commit_error: commit_result.err(),
                };
            }
            SummarizationAttempt::Aborted => {
                diagnostics.push(SummarizationDiagnostic::Finished {
                    succeeded: false,
                    attempts: number,
                    error: Some("aborted".to_owned()),
                });
                return SummarizationRun {
                    outcome: SummarizationOutcome::Failed(SummarizationFailure {
                        kind: SummarizationFailureKind::Aborted,
                        attempts: number,
                        message: "aborted".to_owned(),
                    }),
                    diagnostics,
                    commit_error: None,
                };
            }
            SummarizationAttempt::NonRetryableFailure { message } => {
                diagnostics.push(SummarizationDiagnostic::Finished {
                    succeeded: false,
                    attempts: number,
                    error: Some(message.clone()),
                });
                return SummarizationRun {
                    outcome: SummarizationOutcome::Failed(SummarizationFailure {
                        kind: SummarizationFailureKind::NonRetryable,
                        attempts: number,
                        message,
                    }),
                    diagnostics,
                    commit_error: None,
                };
            }
            SummarizationAttempt::RetryableFailure { message } => {
                if number >= max_attempts {
                    diagnostics.push(SummarizationDiagnostic::Finished {
                        succeeded: false,
                        attempts: number,
                        error: Some(message.clone()),
                    });
                    return SummarizationRun {
                        outcome: SummarizationOutcome::Failed(SummarizationFailure {
                            kind: SummarizationFailureKind::RetriesExhausted,
                            attempts: number,
                            message,
                        }),
                        diagnostics,
                        commit_error: None,
                    };
                }
                let delay = policy.backoff_for_retry(number);
                diagnostics.push(SummarizationDiagnostic::RetryScheduled {
                    attempt: number,
                    max_attempts,
                    delay_ms: delay.as_millis() as u64,
                    error: message,
                });
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                number += 1;
                diagnostics.push(SummarizationDiagnostic::AttemptStart { attempt: number });
            }
        }
    }
}

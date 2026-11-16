//! The built-in tools (`read`, `edit`, `write`, `bash`), the [`CoreTools`]
//! extension that registers them, and the tool-layer durability primitives they
//! need.
//!
//! Core tools implement the same [`Tool`](crate::Tool) trait as third-party
//! tools. [`CoreTools`] additionally marks their native identities so reviewed
//! builtin overrides cannot replace unrelated tools with familiar names.
//!
//! The model-visible core surface is `read`/`write`/`edit`/`bash`. Directory
//! listing, file discovery, and content search use shell commands through
//! `bash`; there are no separate `search`/`ls`/`find`/`grep` tools.
//!
//! Three modules here are not tools but the harness-side primitives Pi defines
//! next to them, landed in the tool layer because `session.rs`/`agent.rs` are
//! outside this change's scope: [`durability`] (durable invocation-scoped
//! partial-output checkpoints and replay memos), [`deferred`] (durable
//! suspend/resume with poll permits), and [`summarization`] (the shared
//! summarization retry policy and its typed outcomes). Each documents the
//! consumer that still has to be wired.

#[cfg(feature = "tools")]
mod bash;
#[cfg(feature = "tools")]
mod edit;
#[cfg(feature = "tools")]
mod powershell;
#[cfg(feature = "tools")]
mod read;
#[cfg(feature = "tools")]
mod shell_environment;
#[cfg(feature = "tools")]
mod write;

#[cfg(feature = "agent")]
pub mod deferred;
pub mod durability;
#[cfg(feature = "agent")]
pub mod summarization;

#[cfg(windows)]
#[cfg(feature = "tools")]
pub use bash::resolve_windows_shell;
#[cfg(feature = "tools")]
pub use bash::{
    BashCheckpointPublisher, BashCheckpointStats, BashTool, CheckpointedBashTool,
    BASH_CHECKPOINT_INTERVAL, BASH_CHECKPOINT_MAX_BYTES, MIN_BASH_CHECKPOINT_INTERVAL,
};
#[cfg(feature = "agent")]
pub use deferred::{
    prepare_deferred_poll, suspend_deferred_response, DeferredHandle, DeferredHandleRejection,
    DeferredPhase, DeferredPollIntent, DeferredPollOutcome, DeferredPollPermit,
    DeferredPollPreparation, DeferredPollRefusal, DeferredPollRefusalKind,
    DeferredResponseDeclaration, DeferredResume, DeferredStopReason, DeferredSuspendDecision,
    DeferredSuspendFailure, DeferredSuspendFailureKind, DeferredSuspended, ModelIdentity,
    SuspendedRunObservation, UnknownPollReplacement, INVALID_DEFERRED_HANDLE_DIAGNOSTIC,
};
pub use durability::{
    DurableInvocationStore, InterruptedInvocation, InvocationError, InvocationHandle,
    InvocationOutcome, InvocationScope, InvocationState, MemoLookup, Settlement, StoreLimits,
    UnsafeRecovery, INTERRUPTED_OUTCOME_UNKNOWN_MARKER, MEMO_NAMESPACE, PARTIAL_OUTPUT_NAMESPACE,
};
#[cfg(feature = "tools")]
pub use edit::EditTool;
#[cfg(feature = "tools")]
pub use powershell::PowerShellTool;
#[cfg(feature = "tools")]
pub use read::ReadTool;
#[cfg(feature = "tools")]
pub use shell_environment::{SessionShellTool, ShellSessionEnvironment};
#[cfg(feature = "agent")]
pub use summarization::{
    run_summarization_with_retry, CompactionFailure, CompactionFailureKind, CompactionStepOutcome,
    SummarizationAttempt, SummarizationDiagnostic, SummarizationFailure, SummarizationFailureKind,
    SummarizationOutcome, SummarizationRetryPolicy, SummarizationRetryScheduled, SummarizationRun,
};
#[cfg(feature = "tools")]
pub use write::WriteTool;

/// Builtin identities eligible for an explicitly reviewed API 0.4 override.
/// Publication still requires the actual CoreTools registration, not just a name.
pub(crate) const BUILTIN_TOOL_NAMES: &[&str] = &["read", "edit", "write", "bash", "powershell"];

#[cfg(feature = "tools")]
mod builtin;
#[cfg(feature = "tools")]
pub use builtin::CoreTools;
#[cfg(feature = "tools")]
pub(crate) use builtin::*;

//! Test suite for the interactive mode, split by concern.
//!
//! Every probe used to live in one inline 7,097-line `mod tests { ... }` block at the
//! bottom of `interactive.rs`. Each module below owns one cohesive area of the host so a
//! single failure names the surface it came from, and `mod support` holds the fixtures
//! more than one area needs so a change to a builder is one edit in one file.

use super::*;
use crate::commands::GoalCommand;
use octet_agent::EntryValue;

// The sibling `onboarding` module builds its theme expectations through this
// fixture at `super::super::tests::terminal_theme_test_config`, so it stays
// reachable at the path it has always used.
pub(super) use support::terminal_theme_test_config;

mod active_reports_and_status_tests;
mod active_run_commands_and_steering_tests;
mod active_subagent_and_queued_control_tests;
mod cache_warming_tests;
mod codex_context_window_tests;
mod compaction_ui_pump_tests;
mod delegated_session_presentation_tests;
mod dialog_and_message_lifecycle_tests;
mod extension_lifecycle_progress_tests;
mod fast_commands_and_compaction_tests;
mod goal_command_tests;
mod held_input_and_manual_compaction_tests;
mod idle_shell_and_clipboard_tests;
mod pi_contract_support;
mod pi_baseline_contract_tests;
mod pi_context_contract_tests;
mod pi_exec_contract_tests;
mod pi_messages_tests;
mod pi_model_provider_tests;
mod pi_tools_surface_tests;
mod pi_ui_contract_tests;
mod pi_helper_import_tests;
mod pi_original_clm_tests;
mod pi_tool_hooks_tests;
mod pi_session_replacement_tests;
mod queued_follow_ups_and_input_close_tests;
mod resource_reload_tests;
mod session_compaction_service_tests;
mod session_head_reconfig_and_startup_tests;
mod shell_escape_and_scoped_models_tests;
mod support;
mod terminal_theme_tests;
mod thinking_and_consent_controls_tests;
mod transcript_search_and_observer_tests;

//! Test suite for the interactive transcript view.
//!
//! The view's own `#[cfg(test)] mod tests;` declaration points here. Every probe used to live
//! in one inline block in `view.rs`; it is split by concern so a single failure names the area
//! it came from. `mod support` holds the fixtures more than one area needs, so a change to the
//! emulated terminal or the theme fixture is one edit in one file.

use std::collections::HashSet;

use sexy_tui_rs::{Block, Inline};

use super::bash_render::render_compact_bash_output;

use super::surface_layout::compile_surface_plan;

use super::tool_render::{
    tool_grid_label, tool_value_indent, tool_value_indent_width, without_redundant_tool_lead,
};

use super::transcript_commit::{
    transcript_commit_cursor, transcript_commit_position, FINAL_COMMIT_SEGMENT,
};

use super::*;

use crate::commands;

use crate::presentation::RunPhase;

use crate::tui::theme::{TerminalBackground, ThemeSurfaceHeading};

use sexy_tui_rs::CURSOR_MARKER;

mod composer_geometry_and_steering_tests;
mod document_panel_and_image_tests;
mod footer_and_cost_tests;
mod frame_repaint_and_viewport_tests;
mod markdown_colour_and_bash_tests;
mod native_history_tests;
mod overlay_and_completion_tests;
mod pi_experience_tests;
mod picker_and_panel_tests;
mod prompt_card_and_chrome_tests;
mod prompt_history_and_paste_tests;
mod provider_retry_and_status_loop_tests;
mod queueing_and_session_misc_tests;
mod reasoning_and_activity_tests;
mod replay_and_native_scrollback_tests;
mod report_readability_tests;
mod scroll_regressions;
mod stream_cache_and_compaction_tests;
mod subagent_rendering_tests;
mod support;
pub(crate) use support::{begin_startup, emulated_shell};
mod terminal_handoff_pty_tests;
mod theme_and_chrome_tests;
mod threaded_native_resize_tests;
mod tool_and_diff_card_tests;
mod transcript_cache_and_resume_tests;
mod transcript_grouping_tests;
mod working_status_and_shimmer_tests;

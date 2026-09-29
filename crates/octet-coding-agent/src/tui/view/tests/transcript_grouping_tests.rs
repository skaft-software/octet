//! Semantic block spacing, expand/collapse, and the grouping of edits and commands. Separate
//! because they own the grouping rules applied across a whole run.

use super::*;

#[test]
fn semantic_transcript_blocks_have_uniform_transition_spacing() {
    let theme = crate::tui::theme::test_theme();
    let rich_renderer = theme.rich_renderer();
    let reasoning_renderer = theme.reasoning_renderer();
    let transcript = (0..12)
        .map(|step| {
            let mut reasoning = AssistantBlock::finalized_reasoning(format!("Step {step}"));
            reasoning.reasoning_expanded = true;
            TranscriptBlock::Reasoning(Box::new(reasoning))
        })
        .collect::<Vec<_>>();

    let mut visible = Vec::new();
    for (index, block) in transcript.iter().enumerate() {
        visible.extend(render_block(
            index.checked_sub(1).and_then(|index| transcript.get(index)),
            block,
            &theme,
            &rich_renderer,
            &reasoning_renderer,
            80,
            false,
        ));
    }
    let plain = visible
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>();
    assert_eq!(
        plain.iter().filter(|line| line.contains("Step ")).count(),
        12
    );
    assert!(plain.iter().any(|line| line.contains("Step 0")));
    assert!(!plain.iter().any(|line| line.contains("earlier analysis")));
    assert_eq!(plain.iter().filter(|line| line.is_empty()).count(), 11);
    for step in 1..12 {
        let label = format!("Step {step}");
        let index = plain
            .iter()
            .position(|line| line.contains(&label))
            .expect("every reasoning block is rendered");
        assert_eq!(
            plain.get(index.wrapping_sub(1)).map(String::as_str),
            Some("")
        );
        assert!(index < 2 || !plain[index - 2].is_empty());
    }

    let mut verbose_reasoning = AssistantBlock::finalized_reasoning(
        "First complete thought.\n\nSecond complete thought.".into(),
    );
    verbose_reasoning.reasoning_expanded = true;
    let verbose = TranscriptBlock::Reasoning(Box::new(verbose_reasoning));
    let verbose = render_block(
        None,
        &verbose,
        &theme,
        &rich_renderer,
        &reasoning_renderer,
        80,
        false,
    )
    .into_iter()
    .map(|line| strip_terminal_sequences(&line))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(verbose.contains("First complete thought."), "{verbose}");
    assert!(verbose.contains("Second complete thought."), "{verbose}");

    let tool = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("read-compact".into()),
        "read".into(),
        serde_json::json!({"path":"src/lib.rs"}).to_string(),
        summarize_tool("read", &serde_json::json!({"path":"src/lib.rs"})),
        String::new(),
        false,
        false,
        None,
        None,
    )));
    let transition = render_block(
        transcript.last(),
        &tool,
        &theme,
        &rich_renderer,
        &reasoning_renderer,
        80,
        false,
    );
    assert_eq!(transition.first().map(String::as_str), Some(""));
    assert!(transition.get(1).is_some_and(|line| !line.is_empty()));
}

#[test]
fn consecutive_tool_calls_have_one_breathing_row_between_them() {
    let theme = crate::tui::theme::test_theme();
    let renderer = theme.rich_renderer();
    let tool = |id: &str, name: &str, args: serde_json::Value| {
        TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId(id.into()),
            name.into(),
            args.to_string(),
            summarize_tool(name, &args),
            String::new(),
            true,
            false,
            None,
            None,
        )))
    };
    let tools = [
        tool("read", "read", serde_json::json!({"path":"src/lib.rs"})),
        tool(
            "bash",
            "bash",
            serde_json::json!({"command":"cargo test -p octet-coding-agent"}),
        ),
        tool("edit", "edit", serde_json::json!({"path":"src/lib.rs"})),
    ];

    for (index, block) in tools.iter().enumerate() {
        let rendered = render_block(
            index
                .checked_sub(1)
                .and_then(|previous| tools.get(previous)),
            block,
            &theme,
            &renderer,
            &renderer,
            80,
            false,
        );
        if index == 0 {
            assert!(rendered.first().is_some_and(|line| !line.is_empty()));
        } else {
            assert_eq!(rendered.first().map(String::as_str), Some(""));
            assert!(rendered.get(1).is_some_and(|line| !line.is_empty()));
            assert!(rendered.get(2).is_none_or(|line| !line.is_empty()));
        }
    }
}

#[test]
fn read_results_stay_hidden_in_collapsed_and_expanded_modes() {
    use octet_agent::ToolOutput;

    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("local");
    let id = ToolCallId("read-hidden-result".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "read".into(),
            args: serde_json::json!({
                "path": "src/private.rs",
                "offset": 41,
                "limit": 7
            }),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: id.clone(),
            result: Ok(ToolOutput::new(
                "READ RESULT SENTINEL\nfn private_implementation() {}",
            )),
            duration: Duration::from_millis(10),
        },
    );
    let transcript = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(100)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let collapsed = transcript(&shell);
    assert!(collapsed.contains("src/private.rs:41-47"), "{collapsed}");
    assert!(!collapsed.contains("READ RESULT SENTINEL"), "{collapsed}");
    assert!(!collapsed.to_ascii_lowercase().contains("evidence"));

    shell.toggle_disclosure();
    let expanded = transcript(&shell);
    assert!(expanded.contains("src/private.rs:41-47"), "{expanded}");
    assert!(!expanded.contains("READ RESULT SENTINEL"), "{expanded}");
    assert!(!expanded.to_ascii_lowercase().contains("evidence"));
    assert_eq!(
        shell.debug_tool_output(&id).as_deref(),
        Some("READ RESULT SENTINEL\nfn private_implementation() {}")
    );
}

#[test]
fn tool_output_tail_expands_with_global_ctrl_o_and_copy_stays_safe() {
    use octet_agent::ToolOutput;
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("local");
    let id = ToolCallId("bash-roundtrip".into());
    let output_lines = (1..=8)
        .map(|line| format!("private result line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let secret =
        format!("exit=0 duration=0.1s\nstdout: 8 lines\n{output_lines}\ntruncated_stdout=false");
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "bash".into(),
            args: serde_json::json!({"command": "printf private"}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: id.clone(),
            result: Ok(ToolOutput::new(secret.clone())),
            duration: Duration::from_millis(10),
        },
    );
    let transcript = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(100)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let collapsed = transcript(&shell);
    assert!(collapsed.contains("private result line 8"), "{collapsed}");
    assert!(!collapsed.contains("private result line 1"), "{collapsed}");
    assert!(collapsed.contains("4 output rows collapsed"), "{collapsed}");
    assert_eq!(
        shell.debug_tool_output(&id).as_deref(),
        Some(secret.as_str())
    );

    shell.toggle_disclosure();
    assert!(shell.verbose_tools());
    let expanded = transcript(&shell);
    assert!(expanded.contains("private result line 1"), "{expanded}");
    assert!(expanded.contains("private result line 8"), "{expanded}");
    assert!(!expanded.to_ascii_lowercase().contains("evidence"));

    shell.toggle_disclosure();
    assert!(!shell.verbose_tools());
    let collapsed_again = transcript(&shell);
    assert!(
        !collapsed_again.contains("private result line 1"),
        "{collapsed_again}"
    );
    let state = shell.state.borrow();
    let index = *state.tool_panels.get(&id).expect("tool panel index");
    assert!(!block_copy_text(&state.transcript[index]).contains("private result line"));
}

#[test]
fn search_output_and_edit_write_diffs_expand_with_global_ctrl_o() {
    use octet_agent::ToolOutput;

    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("local");
    let search_id = ToolCallId("expand-search".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: search_id.clone(),
            name: "search".into(),
            args: serde_json::json!({"query": "needle", "path": "src"}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: search_id,
            result: Ok(ToolOutput::new(
                (1..=8)
                    .map(|line| format!("SEARCH MATCH {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            duration: Duration::from_millis(10),
        },
    );

    for (tool, final_sentinel) in [
        ("edit", "EDIT DIFF FINAL SENTINEL"),
        ("write", "WRITE DIFF FINAL SENTINEL"),
    ] {
        let id = ToolCallId(format!("expand-{tool}"));
        let path = format!("src/{tool}.rs");
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: tool.into(),
                args: serde_json::json!({"path": path}),
            },
        );
        let mut diff = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,12 +1,12 @@\n"
        );
        for line in 1..=11 {
            diff.push_str(&format!("-old {line}\n+new {line}\n"));
        }
        diff.push_str(&format!("-old final\n+{final_sentinel}\n"));
        shell.on_run_event(
            run_id,
            &AgentEvent::ToolFinished {
                id,
                result: Ok(ToolOutput::new(diff)),
                duration: Duration::from_millis(10),
            },
        );
    }

    let transcript = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(120)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let collapsed = transcript(&shell);
    assert!(!collapsed.contains("SEARCH MATCH 1"), "{collapsed}");
    assert!(collapsed.contains("SEARCH MATCH 8"), "{collapsed}");
    assert!(
        !collapsed.contains("EDIT DIFF FINAL SENTINEL"),
        "{collapsed}"
    );
    assert!(
        !collapsed.contains("WRITE DIFF FINAL SENTINEL"),
        "{collapsed}"
    );

    shell.toggle_disclosure();
    let expanded = transcript(&shell);
    assert!(expanded.contains("SEARCH MATCH 1"), "{expanded}");
    assert!(expanded.contains("SEARCH MATCH 8"), "{expanded}");
    assert!(expanded.contains("EDIT DIFF FINAL SENTINEL"), "{expanded}");
    assert!(expanded.contains("WRITE DIFF FINAL SENTINEL"), "{expanded}");
}

#[test]
fn ctrl_o_toggles_all_expandable_transcript_blocks() {
    let mut shell = InteractiveShell::test_shell();
    let args = serde_json::json!({"command": "printf output"});
    let tool_output = (1..=6)
        .map(|line| format!("tool output {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let shell_output = (1..=6)
        .map(|line| format!("shell output {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::finalized_reasoning("private reasoning body".into()),
        )));
        state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
            ToolCallId("global-tool".into()),
            "bash".into(),
            args.to_string(),
            summarize_tool("bash", &args),
            format!("exit=0 duration=0.1s\nstdout: 6 lines\n{tool_output}"),
            true,
            false,
            None,
            None,
        ))));
        state.push_block(TranscriptBlock::Shell(Box::new(ShellOutput {
            id: "global-shell".into(),
            command: "printf shell".into(),
            output: shell_output,
            exit_code: 0,
            running: false,
        })));
        state.push_block(TranscriptBlock::Compaction(Box::new(CompactionBlock {
            label: "Context compacted".into(),
            summary: "private compaction body".into(),
            expanded: false,
        })));
    }
    let transcript = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .rendered_transcript(100)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let collapsed = transcript(&shell);
    assert!(!collapsed.contains("private reasoning body"), "{collapsed}");
    assert!(!collapsed.contains("tool output 1"), "{collapsed}");
    assert!(collapsed.contains("tool output 6"), "{collapsed}");
    assert!(!collapsed.contains("shell output 1"), "{collapsed}");
    assert!(collapsed.contains("shell output 6"), "{collapsed}");
    assert!(
        !collapsed.contains("private compaction body"),
        "{collapsed}"
    );

    shell.toggle_disclosure();
    {
        let mut state = shell.state.borrow_mut();
        state.push_block(TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::finalized_reasoning("future reasoning body".into()),
        )));
    }
    let expanded = transcript(&shell);
    assert!(expanded.contains("private reasoning body"), "{expanded}");
    assert!(expanded.contains("future reasoning body"), "{expanded}");
    assert!(expanded.contains("tool output 1"), "{expanded}");
    assert!(expanded.contains("shell output 1"), "{expanded}");
    assert!(expanded.contains("private compaction body"), "{expanded}");

    shell.toggle_disclosure();
    let collapsed_again = transcript(&shell);
    assert!(
        !collapsed_again.contains("private reasoning body"),
        "{collapsed_again}"
    );
    assert!(
        !collapsed_again.contains("future reasoning body"),
        "{collapsed_again}"
    );
    assert!(
        !collapsed_again.contains("tool output 1"),
        "{collapsed_again}"
    );
    assert!(
        !collapsed_again.contains("shell output 1"),
        "{collapsed_again}"
    );
    assert!(
        !collapsed_again.contains("private compaction body"),
        "{collapsed_again}"
    );
}

#[test]
fn still_activity_labels_are_quiet_for_web_mcp_and_computer_use() {
    use crate::hydrate::{ToolActivityGroup, ToolActivityKind};

    let mut group = ToolActivityGroup::default();
    group.web_searches = 5;
    assert_eq!(
        activity_group_label(&group, &[], ToolActivityKind::WebSearch),
        "Searched web · 5 queries"
    );
    group = ToolActivityGroup::default();
    group.web_fetches = 3;
    assert_eq!(
        activity_group_label(&group, &[], ToolActivityKind::WebFetch),
        "Fetched 3 pages"
    );
    group = ToolActivityGroup::default();
    group.mcp_calls = 2;
    assert_eq!(
        activity_group_label(&group, &[], ToolActivityKind::Mcp),
        "Used MCP · 2 calls"
    );
    group = ToolActivityGroup::default();
    group.computer_use_actions = 4;
    assert_eq!(
        activity_group_label(&group, &[], ToolActivityKind::ComputerUse),
        "Used computer · 4 actions"
    );
}

#[test]
fn live_exploration_groups_follow_turn_finished_and_keep_failures_visible() {
    let theme = crate::tui::theme::test_theme_from_source(
        "[colors]\nquiet_tool_summaries = true\n[surfaces.tool]\nchrome = \"plain\"",
    );
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    let run = shell.begin_run("test");
    let read = ToolCallId("live-read".into());
    let bash = ToolCallId("live-bash".into());
    let call = |id: ToolCallId, name: &str| octet_ai::ToolCall {
        async_execution: false,
        id,
        name: name.into(),
        arguments_json: "{}".into(),
        argument_error: None,
    };
    shell.on_run_event(
        run,
        &AgentEvent::TurnFinished {
            message: octet_ai::AssistantMessage {
                content: vec![
                    octet_ai::AssistantPart::ToolCall(call(read.clone(), "read")),
                    octet_ai::AssistantPart::ToolCall(call(bash.clone(), "bash")),
                ],
                model: ModelId("test".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
            stop_reason: octet_ai::StopReason::ToolUse,
            turn_usage: Usage::default(),
            turn_cost: None,
            usage: Usage::default(),
            session_cost_microdollars: None,
            run_cost_microdollars: 0,
        },
    );
    for (id, name, args) in [
        (&read, "read", serde_json::json!({"path": "src/one.rs"})),
        (&bash, "bash", serde_json::json!({"command": "false"})),
    ] {
        shell.on_run_event(
            run,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: name.into(),
                args,
            },
        );
    }
    shell.on_run_event(
        run,
        &AgentEvent::ToolFinished {
            id: read.clone(),
            result: Ok(octet_agent::ToolOutput::new("content")),
            duration: Duration::from_millis(1),
        },
    );
    shell.on_run_event(
        run,
        &AgentEvent::ToolFinished {
            id: bash.clone(),
            result: Err(octet_agent::ToolError::new("permission denied")),
            duration: Duration::from_millis(1),
        },
    );
    let compact = shell.state.borrow().rendered_transcript(100).join("\n");
    assert!(compact.contains("Explored 1 file · 1 command"), "{compact}");
    assert!(compact.contains("bash: permission denied"), "{compact}");
    assert!(
        !compact.contains("src/one.rs") && !compact.contains("Bash  false"),
        "{compact}"
    );
    shell.set_verbose_tools(true);
    let detailed =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(100).join("\n"));
    assert!(
        detailed.contains("src/one.rs") && detailed.contains("Bash  false"),
        "{detailed}"
    );
}

#[test]
fn still_groups_edits_by_distinct_path_across_responses_and_discloses_failures() {
    let theme = crate::tui::theme::test_theme_from_source(
        "[colors]\nquiet_tool_summaries = true\n[surfaces.tool]\nchrome = \"plain\"",
    );
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    let run = shell.begin_run("test");
    for (number, name, path) in [
        (0, "edit", "src/one.rs"),
        (1, "write", "src/one.rs"),
        (2, "write", "src/two.rs"),
    ] {
        let id = ToolCallId(format!("edit-{number}"));
        let args = serde_json::json!({"path": path, "content": "new"});
        shell.on_run_event(
            run,
            &AgentEvent::TurnFinished {
                message: octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                        async_execution: false,
                        id: id.clone(),
                        name: name.into(),
                        arguments_json: args.to_string(),
                        argument_error: None,
                    })],
                    model: ModelId("test".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
                stop_reason: octet_ai::StopReason::ToolUse,
                turn_usage: Usage::default(),
                turn_cost: None,
                usage: Usage::default(),
                session_cost_microdollars: None,
                run_cost_microdollars: 0,
            },
        );
        shell.on_run_event(
            run,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: name.into(),
                args,
            },
        );
        shell.on_run_event(
            run,
            &AgentEvent::ToolFinished {
                id,
                result: if number == 2 {
                    Err(octet_agent::ToolError::new("permission denied"))
                } else {
                    Ok(octet_agent::ToolOutput::new("ok"))
                },
                duration: Duration::from_millis(1),
            },
        );
    }
    let compact =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(100).join("\n"));
    assert!(compact.contains("Edited 2 files · 1 failed"), "{compact}");
    assert!(compact.contains("write: permission denied"), "{compact}");
    assert!(!compact.contains("src/one.rs"), "{compact}");
    shell.toggle_disclosure();
    let expanded =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(100).join("\n"));
    assert!(
        expanded.contains("src/one.rs") && expanded.contains("src/two.rs"),
        "{expanded}"
    );
}

#[test]
fn still_groups_commands_across_model_responses_and_discloses_each_command() {
    use octet_agent::{EntryId, FinishReason};
    let theme = crate::tui::theme::test_theme_from_source(
        "[colors]\nquiet_tool_summaries = true\n[surfaces.tool]\nchrome = \"plain\"",
    );
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    let run = shell.begin_run("test");
    for (number, command) in ["cargo check", "cargo fmt --check", "cargo test"]
        .into_iter()
        .enumerate()
    {
        let id = ToolCallId(format!("command-{number}"));
        shell.on_run_event(
            run,
            &AgentEvent::TurnFinished {
                message: octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                        async_execution: false,
                        id: id.clone(),
                        name: "bash".into(),
                        arguments_json: serde_json::json!({"command": command}).to_string(),
                        argument_error: None,
                    })],
                    model: ModelId("test".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
                stop_reason: octet_ai::StopReason::ToolUse,
                turn_usage: Usage::default(),
                turn_cost: None,
                usage: Usage::default(),
                session_cost_microdollars: None,
                run_cost_microdollars: 0,
            },
        );
        shell.on_run_event(
            run,
            &AgentEvent::ToolStarted {
                id: id.clone(),
                name: "bash".into(),
                args: serde_json::json!({"command": command}),
            },
        );
        shell.on_run_event(
            run,
            &AgentEvent::ToolFinished {
                id,
                result: if number == 2 {
                    Err(octet_agent::ToolError::new("test failed"))
                } else {
                    Ok(octet_agent::ToolOutput::new("ok"))
                },
                duration: Duration::from_millis(1),
            },
        );
    }
    {
        let state = shell.state.borrow();
        state.rendered_transcript(100);
        let index = state.transcript.iter().position(|block| matches!(block, TranscriptBlock::NoticeStatus { text, .. } if text.starts_with("Ran 3 Commands"))).unwrap();
        let cursor = transcript_commit_cursor(&state, index, FINAL_COMMIT_SEGMENT);
        assert!(transcript_commit_position(&state, cursor).is_none());
    }
    shell.on_run_event(
        run,
        &AgentEvent::RunFinished {
            head: EntryId("head".into()),
            reason: FinishReason::Completed,
        },
    );
    let compact =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(100).join("\n"))
            .to_owned();
    assert!(compact.contains("Ran 3 Commands"), "{compact}");
    assert!(compact.contains("test failed"), "{compact}");
    assert!(!compact.contains("cargo check"), "{compact}");
    shell.set_verbose_tools(true);
    let expanded =
        strip_terminal_sequences(&shell.state.borrow().rendered_transcript(100).join("\n"))
            .to_owned();
    for command in ["cargo check", "cargo fmt --check", "cargo test"] {
        assert!(
            expanded.contains(&format!("-> Bash  {command}")),
            "{expanded}"
        );
    }
}

#[test]
fn still_shows_one_tool_summary_until_disclosure_without_hiding_failures() {
    let theme = crate::tui::theme::test_theme_from_source(
        "[colors]\nquiet_tool_summaries = true\n[surfaces.tool]\nchrome = \"plain\"",
    );
    let renderer = theme.rich_renderer();
    let args = serde_json::json!({"path": "src/lib.rs"});
    let edit = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("quiet-edit".into()),
        "edit".into(),
        args.to_string(),
        summarize_tool("edit", &args),
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new".into(),
        true,
        false,
        None,
        None,
    )));
    let render = |block: &TranscriptBlock, expanded| {
        strip_terminal_sequences(
            &render_block_planned(
                None, block, &theme, &renderer, &renderer, 100, expanded, 0, 0,
            )
            .lines
            .join("\n"),
        )
        .to_owned()
    };
    for width in [48, 100, 160] {
        let compact = strip_terminal_sequences(
            &render_block_planned(
                None, &edit, &theme, &renderer, &renderer, width, false, 0, 0,
            )
            .lines
            .join("\n"),
        );
        assert!(
            compact.contains("Edit")
                && compact.contains("lib.rs")
                && !compact.contains("src/lib.rs"),
            "{width}: {compact}"
        );
        assert!(!compact.contains("+new"), "{width}: {compact}");
    }
    assert!(render(&edit, true).contains("+new"));

    let failed = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("quiet-failure".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        String::new(),
        true,
        true,
        Some("permission denied".into()),
        None,
    )));
    assert!(render(&failed, false).contains("permission denied"));

    let failed_edit = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("quiet-failed-edit".into()),
        "edit".into(),
        args.to_string(),
        summarize_tool("edit", &args),
        String::new(),
        true,
        true,
        Some("src/lib.rs: no match".into()),
        None,
    )));
    let compact_failure = render(&failed_edit, false);
    assert!(
        compact_failure.contains("lib.rs: no match"),
        "{compact_failure}"
    );
    assert!(!compact_failure.contains("src/lib.rs"), "{compact_failure}");
    assert!(render(&failed_edit, true).contains("src/lib.rs: no match"));
}

#[test]
fn theme_can_remove_activity_tree_connectors() {
    let theme = crate::tui::theme::test_theme_from_source(
        "[glyphs]\nlast_branch = \" \"\nvertical = \" \"\n[glyphs_ascii]\nlast_branch = \" \"\nvertical = \" \"",
    );
    assert_eq!(activity_elbow(&theme), " ");
    let args = serde_json::json!({"path": "src/lib.rs"});
    let panel = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("failed-read".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        "permission denied".into(),
        true,
        true,
        Some("permission denied".into()),
        None,
    )));
    let renderer = theme.rich_renderer();
    let rows = render_block(None, &panel, &theme, &renderer, &renderer, 80, false);
    let plain = rows
        .iter()
        .map(|row| strip_terminal_sequences(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plain.contains("permission denied"), "{plain}");
    assert!(
        !plain.contains('└') && !plain.contains('│') && !plain.contains("`-"),
        "{plain}"
    );
}

#[test]
fn extension_tool_renderer_stays_internal_to_the_tool_record() {
    use octet_agent::extension_process::ToolRenderSegment;
    use octet_agent::ToolOutput;
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("local");
    let id = ToolCallId("extension-render".into());
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolStarted {
            id: id.clone(),
            name: "git_status".into(),
            args: serde_json::json!({"workspace": "."}),
        },
    );
    shell.on_run_event(
        run_id,
        &AgentEvent::ToolFinished {
            id: id.clone(),
            result: Ok(ToolOutput::new("RAW EVIDENCE")),
            duration: Duration::from_millis(10),
        },
    );
    shell.apply_extension_tool_renderer(
        &id,
        &[ToolRenderSegment {
            text: "branch: main".into(),
            style_role: Some("extension.test.label".into()),
        }],
    );
    let rendered = shell
        .state
        .borrow()
        .rendered_transcript(100)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("RAW EVIDENCE"), "{rendered}");
    assert!(!rendered.contains("branch: main"), "{rendered}");
    shell.toggle_disclosure();
    let expanded = shell
        .state
        .borrow()
        .rendered_transcript(100)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!expanded.contains("RAW EVIDENCE"), "{expanded}");
    assert!(!expanded.contains("branch: main"), "{expanded}");
    let state = shell.state.borrow();
    let index = *state.tool_panels.get(&id).expect("tool panel index");
    let TranscriptBlock::Tool(panel) = &state.transcript[index] else {
        panic!("tool panel")
    };
    assert_eq!(panel.output, "RAW EVIDENCE");
    assert_eq!(panel.extension_render_segments[0].text, "branch: main");
}

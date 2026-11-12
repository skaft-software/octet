use super::*;

#[test]
fn shell_output_copy_preserves_sanitized_source() {
    let block = TranscriptBlock::Shell(Box::new(super::super::ShellOutput {
        id: "copy-fixture".into(),
        command: "printf fixture".into(),
        output: "\x1b[31mCOPY-ASCII-8B0C\x1b[0m\n界é-UNICODE-8B0C\n".into(),
        exit_code: 0,
        running: false,
    }));
    let text = block_copy_text(&block);
    assert_eq!(
        text,
        "$ printf fixture [completed]\nCOPY-ASCII-8B0C\n界é-UNICODE-8B0C\n"
    );
}

#[test]
fn shell_output_drag_maps_painted_ascii_and_unicode_not_command_header() {
    use super::super::InteractiveShell;
    for threaded in [false, true] {
        for (width, exit_code) in [(100, None), (100, Some(0)), (42, Some(1))] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, 30);
            shell.state.borrow_mut().application_viewport_requested = true;
            let command = "printf 'COPY-ASCII-8B0C\\n界é-UNICODE-8B0C\\n'";
            shell.on_prompt_submitted(&format!("!{command}"));
            let id = shell.append_shell_in_progress(command.into());
            let output = "COPY-ASCII-8B0C\n界é-UNICODE-8B0C\n";
            if let Some(exit_code) = exit_code {
                shell.finalize_shell(&id, output.into(), exit_code);
            } else {
                shell.update_shell_output(&id, output.into());
            }
            if threaded {
                shell.isolate_native_test_renderer();
            }
            let mut selection_start = None;
            for expected in ["COPY-ASCII-8B0C", "界é-UNICODE-8B0C"] {
                let lines = if threaded {
                    shell.render_written_test_frame();
                    shell
                        .state
                        .borrow()
                        .pointer_render_geometry()
                        .unwrap()
                        .visible_lines
                        .clone()
                } else {
                    super::super::render_shell_viewport_at(
                        &shell.state.borrow(),
                        width,
                        std::time::Instant::now(),
                    )
                };
                let (row, col) = lines
                    .iter()
                    .enumerate()
                    .find_map(|(row, line)| {
                        let plain = strip_terminal_sequences(line);
                        if !plain.ends_with(expected) {
                            return None;
                        }
                        let offset = plain.len() - expected.len();
                        Some((row as u16, visible_width(&plain[..offset]) as u16))
                    })
                    .expect("output is painted");
                shell.begin_transcript_selection(row, col, false);
                for step in 1..=visible_width(expected) as u16 {
                    shell.extend_transcript_selection(row, col + step);
                }
                assert_eq!(
                    shell.selected_plain_text().as_deref(),
                    Some(expected),
                    "threaded={threaded}, width={width}, exit_code={exit_code:?}"
                );
                if let Some((start_row, start_col)) = selection_start {
                    shell.begin_transcript_selection(start_row, start_col, false);
                    shell.extend_transcript_selection(row, col + visible_width(expected) as u16);
                    assert_eq!(
                        shell.selected_plain_text().as_deref(),
                        Some(output.trim_end())
                    );
                } else {
                    selection_start = Some((row, col));
                }
            }
        }
    }
}

#[test]
fn shell_output_drag_maps_wrapped_rows_without_consuming_unpainted_source() {
    use super::super::InteractiveShell;
    for threaded in [false, true] {
        for width in [32, 48] {
            for output in [
                "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-END",
                "界é0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ界éabcdefghijklmnopqrstuvwxyz-END",
            ] {
                let mut shell = InteractiveShell::test_shell();
                shell.set_size(width, 40);
                shell.state.borrow_mut().application_viewport_requested = true;
                let command = "printf WRAPPED-FIXTURE";
                let id = shell.append_shell_in_progress(command.into());
                shell.finalize_shell(&id, output.into(), 0);
                if threaded {
                    shell.isolate_native_test_renderer();
                }
                let lines = if threaded {
                    shell.render_written_test_frame();
                    shell
                        .state
                        .borrow()
                        .pointer_render_geometry()
                        .unwrap()
                        .visible_lines
                        .clone()
                } else {
                    super::super::render_shell_viewport_at(
                        &shell.state.borrow(),
                        width,
                        std::time::Instant::now(),
                    )
                };
                let mut output_rows = 0;
                for (row, line) in lines.iter().enumerate() {
                    let plain = strip_terminal_sequences(line);
                    let expected = plain.trim().trim_start_matches('└').trim_start();
                    if expected.is_empty() || !output.contains(expected) {
                        continue;
                    }
                    output_rows += 1;
                    let prefix = plain.len() - plain.trim_start().len();
                    let offset = plain[prefix..].find(expected).unwrap() + prefix;
                    let col = visible_width(&plain[..offset]) as u16;
                    shell.begin_transcript_selection(row as u16, col, false);
                    shell.extend_transcript_selection(
                        row as u16,
                        col + visible_width(expected) as u16,
                    );
                    assert_eq!(
                        shell.selected_plain_text().as_deref(),
                        Some(expected),
                        "threaded={threaded}, width={width}, row={row}, output={output}"
                    );
                }
                assert!(output_rows >= 2, "fixture must wrap into multiple rows");
            }
        }
    }
}

#[test]
fn compact_shell_selection_starts_at_the_retained_tail_not_an_omitted_duplicate() {
    use super::super::InteractiveShell;
    for threaded in [false, true] {
        for (width, hidden) in [
            (48, "omitted"),
            (80, "omitted"),
            (48, "… 3 earlier visual rows hidden"),
            (80, "… 3 earlier visual rows hidden"),
        ] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, 30);
            shell.state.borrow_mut().application_viewport_requested = true;
            let id = shell.append_shell_in_progress("printf fixture".into());
            let tail = "SAME 界é\ntail-1\ntail-2\nEND";
            let output = format!("SAME 界é\nPRIVATE-HIDDEN\n{hidden}\n{tail}\n");
            shell.finalize_shell(&id, output, 0);
            if threaded {
                shell.isolate_native_test_renderer();
            }
            let lines = if threaded {
                shell.render_written_test_frame();
                shell
                    .state
                    .borrow()
                    .pointer_render_geometry()
                    .unwrap()
                    .visible_lines
                    .clone()
            } else {
                super::super::render_shell_viewport_at(
                    &shell.state.borrow(),
                    width,
                    std::time::Instant::now(),
                )
            };
            let cell = |needle: &str| {
                lines
                    .iter()
                    .enumerate()
                    .find_map(|(row, line)| {
                        let plain = strip_terminal_sequences(line);
                        let offset = plain.find(needle)?;
                        Some((row as u16, visible_width(&plain[..offset]) as u16))
                    })
                    .expect("fixture row painted")
            };
            let (start_row, start_col) = cell("SAME 界é");
            let (end_row, end_col) = cell("END");
            shell.begin_transcript_selection(start_row, start_col, false);
            shell.extend_transcript_selection(end_row, end_col + 3);
            assert_eq!(
                shell.selected_plain_text().as_deref(),
                Some(tail),
                "threaded={threaded}, width={width}"
            );
            let (row, col) = cell("earlier visual rows hidden");
            shell.state.borrow_mut().transcript_selection = None;
            shell.begin_transcript_selection(row, col, false);
            shell.extend_transcript_selection(row, col + 7);
            assert_eq!(shell.selected_plain_text().as_deref().unwrap_or(""), "");
        }
    }
}

#[test]
fn compact_wrapped_shell_tail_keeps_its_source_boundary_across_reflow() {
    use super::super::InteractiveShell;
    for threaded in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.state.borrow_mut().application_viewport_requested = true;
        let id = shell.append_shell_in_progress("printf fixture".into());
        let output = format!("{}END", "界é0123456789".repeat(36));
        shell.finalize_shell(&id, output.clone(), 0);
        if threaded {
            shell.isolate_native_test_renderer();
        }
        for width in [32, 48, 32] {
            shell.set_size(width, 30);
            let lines = if threaded {
                shell.render_written_test_frame();
                shell
                    .state
                    .borrow()
                    .pointer_render_geometry()
                    .unwrap()
                    .visible_lines
                    .clone()
            } else {
                super::super::render_shell_viewport_at(
                    &shell.state.borrow(),
                    width,
                    std::time::Instant::now(),
                )
            };
            let rows: Vec<_> = lines
                .iter()
                .enumerate()
                .filter_map(|(row, line)| {
                    let plain = strip_terminal_sequences(line);
                    let text = plain.trim();
                    if text.is_empty() || !output.contains(text) {
                        return None;
                    }
                    let column = visible_width(&plain[..plain.find(text).unwrap()]) as u16;
                    Some((row as u16, column, text.to_owned()))
                })
                .collect();
            assert_eq!(rows.len(), super::super::COMPACT_EXEC_OUTPUT_ROWS - 1);
            let first = rows.first().unwrap();
            let last = rows.last().unwrap();
            shell.begin_transcript_selection(first.0, first.1, false);
            shell.extend_transcript_selection(last.0, last.1 + visible_width(&last.2) as u16);
            assert_eq!(
                shell.selected_plain_text(),
                Some(rows.iter().map(|row| row.2.as_str()).collect())
            );
        }
    }
}

#[test]
fn bounded_copy_rows_match_full_receipt_at_scrolled_unicode_cells() {
    let content: Vec<_> = (0..120).map(|row| format!("row-{row:03} 界 é")).collect();
    let text = content.join("\n");
    let mut painted = vec![String::new()];
    painted.extend(content.iter().map(|line| format!("  {line}")));
    painted.push(String::new());
    let geometry = SurfaceGeometry {
        leading_rows: 1,
        trailing_rows: 1,
        content_left: 2,
        ..Default::default()
    };
    let full = CopyRows::build(&painted, &text, geometry, None);
    let bounded = visible_copy_rows(
        &painted,
        &text,
        geometry,
        None,
        90..96,
        &mut Default::default(),
    )
    .unwrap();
    assert_eq!(bounded.first_row, 89);
    assert_eq!(bounded.rows.len(), 6);
    for row in 89..95 {
        for col in 0..18 {
            assert_eq!(bounded.offset_for(row, col), full.offset_for(row, col));
        }
        let offset = full.offset_for(row, 1).unwrap();
        assert_eq!(
            bounded.row_for_offset(offset, false),
            full.row_for_offset(offset, false)
        );
    }
    assert!(bounded.offset_for(88, 0).is_none());
    assert!(bounded.offset_for(95, 0).is_none());
    assert!(visible_copy_rows(
        &painted,
        &text,
        geometry,
        None,
        0..1,
        &mut Default::default()
    )
    .is_none());
    assert!(visible_copy_rows(
        &painted,
        &text,
        geometry,
        None,
        121..122,
        &mut Default::default()
    )
    .is_none());
}

#[test]
fn selection_decoration_preserves_plain_unicode_and_leaves_blank_rows_alone() {
    let row = CopyRow {
        text: "a界éz".into(),
        offset: 7,
        painted_cells: 2,
    };
    let mut line = "  a界éz".to_owned();
    row.decorate(&mut line, 8, 13);
    assert_eq!(strip_terminal_sequences(&line), "  a界éz");
    assert!(line.contains("\x1b[0;7m界é\x1b[0m"));
    let empty = CopyRow {
        text: String::new(),
        offset: 7,
        painted_cells: 2,
    };
    let mut blank = "  ".to_owned();
    empty.decorate(&mut blank, 0, 100);
    assert_eq!(blank, "  ");
}

#[test]
fn memoized_copy_rows_match_a_fresh_alignment_through_a_stream() {
    // A painter shaped like the transcript: bullets, styled rows, frame rows
    // that are not copy text, and prose re-wrapped as the answer grows.
    fn paint(text: &str) -> Vec<String> {
        let mut painted = vec![String::new()];
        for (index, line) in text.split('\n').enumerate() {
            // Headings paint a marker the copy text spells differently, so
            // the whole-row candidate misses and the line candidate matches.
            let (marker, body) = if let Some(body) = line.strip_prefix("- ") {
                ("• ", body)
            } else if let Some(body) = line.strip_prefix("## ") {
                ("* ", body)
            } else {
                ("", line)
            };
            let words = body.split(' ').collect::<Vec<_>>();
            for (chunk, words) in words.chunks(5).enumerate() {
                let lead = if chunk == 0 { marker } else { "  " };
                let row = format!("  {lead}{}", words.join(" "));
                painted.push(if index % 3 == 0 {
                    format!("\x1b[1m{row}\x1b[0m")
                } else {
                    row
                });
            }
            if index % 4 == 3 {
                painted.push("  ────────".into());
            }
        }
        painted.push(String::new());
        painted
    }
    let words = (0..400)
        .map(|word| match word % 37 {
            0 => "\n- item".to_owned(),
            11 => format!("\n## head{word}\n"),
            19 => "\nprose".to_owned(),
            // A later literal spelling of a painted heading row.
            29 => format!("* head{}", word - 18),
            _ => format!("w{word}"),
        })
        .collect::<Vec<_>>();
    let full = words.join(" ");
    let geometry = SurfaceGeometry {
        leading_rows: 1,
        trailing_rows: 1,
        content_left: 2,
        ..Default::default()
    };
    let mut memo = BlockCopyMemo::default();
    let mut end = 0;
    while end < full.len() {
        end = (end + 7).min(full.len());
        while !full.is_char_boundary(end) {
            end += 1;
        }
        let text = &full[..end];
        let painted = paint(text);
        let rows = painted.len();
        for visible in [rows.saturating_sub(12)..rows, 0..rows, 3..9] {
            let fresh = CopyRows::build_visible(&painted, text, geometry, None, visible.clone());
            let memoized = CopyRows::build_visible_with(
                &painted,
                text,
                geometry,
                None,
                visible.clone(),
                &mut memo,
            );
            assert_eq!(
                format!("{memoized:?}"),
                format!("{fresh:?}"),
                "copy rows diverged at {end} bytes for rows {visible:?}"
            );
        }
    }
    // A changed inset discards every remembered row.
    let painted = paint(&full);
    let moved = SurfaceGeometry {
        content_left: 4,
        ..geometry
    };
    assert_eq!(
        format!(
            "{:?}",
            CopyRows::build_visible_with(&painted, &full, moved, None, 0..painted.len(), &mut memo)
        ),
        format!("{:?}", CopyRows::build(&painted, &full, moved, None))
    );
}

fn selection_frame(shell: &mut super::super::InteractiveShell, threaded: bool) -> Vec<String> {
    if threaded {
        shell.render_written_test_frame();
        shell
            .state
            .borrow()
            .pointer_render_geometry()
            .unwrap()
            .visible_lines
            .clone()
    } else {
        let state = shell.state.borrow();
        super::super::render_shell_viewport_at(&state, state.size.0, std::time::Instant::now())
    }
}

fn selection_cell(lines: &[String], needle: &str) -> (u16, u16) {
    lines
        .iter()
        .enumerate()
        .find_map(|(row, line)| {
            let plain = strip_terminal_sequences(line);
            let offset = plain.find(needle)?;
            Some((row as u16, visible_width(&plain[..offset]) as u16))
        })
        .unwrap_or_else(|| panic!("missing {needle:?} in {lines:?}"))
}

#[test]
fn decorated_duplicate_rows_copy_the_selected_occurrence() {
    use super::super::{AssistantBlock, InteractiveShell};
    let repeated = "repeat 界e\u{301}👩‍💻";
    let expected = format!("{repeated}\n- target");
    for threaded in [false, true] {
        for width in [48, 96] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, 30);
            shell.state.borrow_mut().application_viewport_requested = true;
            shell
                .state
                .borrow_mut()
                .push_block(TranscriptBlock::Assistant(Box::new(
                    AssistantBlock::finalized(format!(
                        "- {repeated}\n- target\n\nliteral — {repeated}\n\nUNSELECTED"
                    )),
                )));
            if threaded {
                shell.isolate_native_test_renderer();
            }
            let lines = selection_frame(&mut shell, threaded);
            let a = selection_cell(&lines, repeated);
            let b = selection_cell(&lines, "target");
            for reverse in [false, true] {
                let (start, end) = if reverse {
                    ((b.0, b.1 + 6), a)
                } else {
                    (a, (b.0, b.1 + 6))
                };
                shell.begin_transcript_selection(start.0, start.1, false);
                shell.extend_transcript_selection(end.0, end.1);
                let actual = shell.selected_plain_text();
                {
                    let state = shell.state.borrow();
                    assert_eq!(actual.as_deref(), Some(expected.as_str()),
                        "threaded={threaded}, width={width}, reverse={reverse}, a={a:?}, b={b:?}, lines={lines:?}, selection={:?}, pending={:?}, copy={:?}",
                        state.transcript_selection, state.pending_selection_anchor,
                        block_copy_text(&state.transcript[0]));
                }
                // Pointer receipts intentionally retain undecorated rows.
                let decorated = selection_frame(&mut shell, false);
                assert!(
                    decorated.iter().any(|line| line.contains("\x1b[0;7m")),
                    "selection must be painted: {decorated:?}"
                );
            }
        }
    }
}

#[test]
fn disclosed_tool_output_drag_copies_only_presented_text() {
    use super::super::{InteractiveShell, ToolPanel};
    use crate::presentation::summarize_tool;
    use octet_ai::ToolCallId;
    let expected = "VALUE 界e\u{301}👩‍💻";
    for threaded in [false, true] {
        for width in [48, 96] {
            for name in ["bash", "codemode", "edit"] {
                let mut shell = InteractiveShell::test_shell();
                shell.set_size(width, 30);
                shell.state.borrow_mut().application_viewport_requested = true;
                let args = match name {
                    "bash" => {
                        serde_json::json!({"command": "printf fixture", "secret": "PRIVATE-ARGS"})
                    }
                    "codemode" => {
                        serde_json::json!({"code": "text('fixture');", "secret": "PRIVATE-ARGS"})
                    }
                    _ => {
                        serde_json::json!({"path": "fixture.txt", "old_text": "old", "new_text": expected, "secret": "PRIVATE-ARGS"})
                    }
                };
                let output = if name == "bash" {
                    format!(
                        "exit=0 duration=0.01s\nstdout:\n\x1b[31m{expected}\x1b[0m\nSECOND\ncomplete_stdout=true"
                    )
                } else {
                    format!("\x1b[31m{expected}\x1b[0m\nSECOND")
                };
                let mut panel = ToolPanel::new(
                    ToolCallId("selection-tool".into()),
                    name.into(),
                    args.to_string(),
                    summarize_tool(name, &args),
                    output,
                    true,
                    false,
                    None,
                    None,
                );
                if name == "edit" {
                    panel.diff = Some(format!(
                        "--- a/fixture.txt\n+++ b/fixture.txt\n@@ -1 +1 @@\n-old\n+{expected}\n"
                    ));
                }
                shell
                    .state
                    .borrow_mut()
                    .push_block(TranscriptBlock::Tool(Box::new(panel)));
                shell.toggle_disclosure();
                if threaded {
                    shell.isolate_native_test_renderer();
                }
                let lines = selection_frame(&mut shell, threaded);
                let (row, col) = selection_cell(&lines, expected);
                for reverse in [false, true] {
                    let end = col + visible_width(expected) as u16;
                    let (from, to) = if reverse { (end, col) } else { (col, end) };
                    shell.begin_transcript_selection(row, from, false);
                    shell.extend_transcript_selection(row, to);
                    assert_eq!(
                        shell.selected_plain_text().as_deref(),
                        Some(expected),
                        "threaded={threaded}, width={width}, tool={name}, reverse={reverse}"
                    );
                    let highlighted = selection_frame(&mut shell, false);
                    assert!(
                        highlighted[usize::from(row)]
                            .contains(&format!("\x1b[0;7m{expected}\x1b[0m")),
                        "{highlighted:?}"
                    );
                }
                shell.select_all_transcript();
                let all = shell.selected_plain_text().unwrap();
                assert!(all.contains(expected));
                assert!(
                    !all.contains("PRIVATE-ARGS")
                        && !all.contains("complete_stdout")
                        && !all.contains("Ctrl+O"),
                    "{all}"
                );
            }
        }
    }
}

fn selection_panel(name: &str, args: serde_json::Value, output: String) -> super::super::ToolPanel {
    super::super::ToolPanel::new(
        octet_ai::ToolCallId(format!("copy-{name}")),
        name.into(),
        args.to_string(),
        crate::presentation::summarize_tool(name, &args),
        output,
        true,
        false,
        None,
        None,
    )
}

#[test]
fn tool_copy_whitespace_wrapped_bash_output_preserves_source() {
    use super::super::InteractiveShell;
    let output = format!("{}foo\nbar", " ".repeat(200));
    for threaded in [false, true] {
        for width in [80, 100] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, 24);
            shell.state.borrow_mut().application_viewport_requested = true;
            shell
                .state
                .borrow_mut()
                .push_block(TranscriptBlock::Tool(Box::new(selection_panel(
                    "bash",
                    serde_json::json!({"command":"printf fixture"}),
                    output.clone(),
                ))));
            if threaded {
                shell.isolate_native_test_renderer();
            }
            let frame = selection_frame(&mut shell, threaded);
            let foo = selection_cell(&frame, "foo");
            let bar = selection_cell(&frame, "bar");
            shell.begin_transcript_selection(foo.0, foo.1, false);
            shell.extend_transcript_selection(bar.0, bar.1 + 3);
            assert_eq!(shell.selected_plain_text().as_deref(), Some("foo\nbar"));
            shell.select_all_transcript();
            assert_eq!(
                shell.selected_plain_text().as_deref(),
                Some(format!("$ printf fixture\n{output}").as_str()),
                "threaded={threaded}, width={width}"
            );
        }
    }
}

#[test]
fn tool_copy_long_label_contains_no_terminal_sequences() {
    use super::super::InteractiveShell;
    let name = "an_extremely_long_tool_name";
    for threaded in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.state.borrow_mut().application_viewport_requested = true;
        let mut panel = selection_panel(name, serde_json::json!({}), String::new());
        panel.display.value = Some("COPY-VALUE".into());
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Tool(Box::new(panel)));
        if threaded {
            shell.isolate_native_test_renderer();
        }
        let frame = selection_frame(&mut shell, threaded);
        let value = selection_cell(&frame, "COPY-VALUE");
        shell.begin_transcript_selection(value.0, value.1, false);
        shell.extend_transcript_selection(value.0, value.1 + 10);
        assert_eq!(shell.selected_plain_text().as_deref(), Some("COPY-VALUE"));
        shell.select_all_transcript();
        let copied = shell.selected_plain_text().unwrap();
        assert!(!copied.contains('\x1b'), "copy contains ANSI: {copied:?}");
        assert_eq!(copied, "An extremely long tool name  COPY-VALUE");
    }
}

#[test]
fn compact_tool_copy_excludes_undisclosed_rows_but_not_offscreen_rows() {
    use super::super::{InteractiveShell, TranscriptBlock};
    for threaded in [false, true] {
        for width in [48, 96] {
            for name in ["bash", "codemode", "edit"] {
                let mut shell = InteractiveShell::test_shell();
                shell.set_size(width, 40);
                shell.state.borrow_mut().application_viewport_requested = true;
                let args = match name {
                    "bash" => {
                        serde_json::json!({"command":"printf fixture", "private":"PRIVATE-ARGS"})
                    }
                    "codemode" => {
                        serde_json::json!({"code":"text('fixture');", "private":"PRIVATE-ARGS"})
                    }
                    _ => serde_json::json!({"path":"fixture.txt", "private":"PRIVATE-ARGS"}),
                };
                let body = (0..24)
                    .map(|row| {
                        if row == 12 {
                            "UNDISCLOSED 界e\u{301}".into()
                        } else if row == 0 || row == 22 {
                            "DUP 界e\u{301}".into()
                        } else if row == 23 {
                            "TAIL".into()
                        } else {
                            format!("value-{row:02}")
                        }
                    })
                    .collect::<Vec<String>>()
                    .join("\n");
                let output = if name == "bash" {
                    format!("exit=0 duration=0.01s\nstdout:\n{body}\ncomplete_stdout=true\nfull_output_path=/PRIVATE-CAPTURE")
                } else {
                    body.clone()
                };
                let mut panel = selection_panel(name, args, output);
                if name == "edit" {
                    panel.diff = Some(format!(
                        "--- a/fixture.txt\n+++ b/fixture.txt\n@@ -0,0 +1,24 @@\n+{}\n",
                        body.replace('\n', "\n+")
                    ));
                }
                shell
                    .state
                    .borrow_mut()
                    .push_block(TranscriptBlock::Tool(Box::new(panel)));
                if threaded {
                    shell.isolate_native_test_renderer();
                }
                let compact = selection_frame(&mut shell, threaded);
                assert!(!strip_terminal_sequences(&compact.join("\n")).contains("UNDISCLOSED"));
                shell.select_all_transcript();
                let all = shell.selected_plain_text().unwrap();
                assert!(all.contains("DUP 界e\u{301}"), "{name}: {all}");
                for hidden in [
                    "UNDISCLOSED",
                    "PRIVATE-ARGS",
                    "PRIVATE-CAPTURE",
                    "complete_stdout",
                    "Ctrl+O",
                    "collapsed",
                    "hidden",
                ] {
                    assert!(!all.contains(hidden), "{name}: {all}");
                }
                // A compact Bash tail repeats the head. Selecting its occurrence
                // must not bridge the unpainted middle to the earlier duplicate.
                if name == "bash" {
                    let tail = selection_cell(&compact, "TAIL");
                    let head = compact
                        .iter()
                        .enumerate()
                        .rev()
                        .find_map(|(row, line)| {
                            let plain = strip_terminal_sequences(line);
                            let offset = plain.find("DUP 界e\u{301}")?;
                            Some((row as u16, visible_width(&plain[..offset]) as u16))
                        })
                        .unwrap();
                    shell.begin_transcript_selection(head.0, head.1, false);
                    shell.extend_transcript_selection(tail.0, tail.1 + 4);
                    assert_eq!(
                        shell.selected_plain_text().as_deref(),
                        Some("DUP 界e\u{301}\nTAIL")
                    );
                }
                shell.toggle_disclosure();
                shell.set_size(width, 12);
                let expanded_tail = selection_frame(&mut shell, threaded);
                assert!(
                    !strip_terminal_sequences(&expanded_tail.join("\n")).contains("UNDISCLOSED"),
                    "middle must be offscreen"
                );
                shell.select_all_transcript();
                let all = shell.selected_plain_text().unwrap();
                assert!(all.contains("UNDISCLOSED 界e\u{301}"), "{name}: {all}");
                assert!(all.contains("TAIL"));
                assert!(
                    !all.contains("PRIVATE-ARGS") && !all.contains("PRIVATE-CAPTURE"),
                    "{all}"
                );
                shell.toggle_disclosure();
                let _ = selection_frame(&mut shell, threaded);
                shell.select_all_transcript();
                assert!(!shell.selected_plain_text().unwrap().contains("UNDISCLOSED"));
            }
        }
    }
}

#[test]
fn source_previews_and_grouped_children_follow_disclosure_for_copy() {
    use super::super::{AssistantBlock, CompactionBlock, InteractiveShell};
    for threaded in [false, true] {
        for width in [48, 96] {
            let theme = crate::tui::theme::test_theme_from_source(
                "[colors]\nquiet_tool_summaries = true\n[surfaces.tool]\nchrome = \"plain\"",
            );
            let mut shell = InteractiveShell::test_shell_with_theme(theme);
            shell.set_size(width, 40);
            shell.state.borrow_mut().application_viewport_requested = true;
            let mut panel = selection_panel(
                "bash",
                serde_json::json!({"command":"printf 'GROUPED 界e\u{301}'", "private":"PRIVATE-ARGS"}),
                "GROUPED-OUTPUT".into(),
            );
            panel.grouped_child = true;
            {
                let mut state = shell.state.borrow_mut();
                state.push_block(TranscriptBlock::Tool(Box::new(panel)));
                state.push_block(TranscriptBlock::Reasoning(Box::new(
                    AssistantBlock::finalized_reasoning("UNDISCLOSED-REASONING".into()),
                )));
                state.push_block(TranscriptBlock::Compaction(Box::new(CompactionBlock {
                    label: "Context compacted".into(),
                    summary: "UNDISCLOSED-SUMMARY".into(),
                    expanded: false,
                })));
                state.push_block(TranscriptBlock::Notice("VISIBLE-NOTE".into()));
            }
            if threaded {
                shell.isolate_native_test_renderer();
            }
            let _ = selection_frame(&mut shell, threaded);
            shell.select_all_transcript();
            assert_eq!(
                shell.selected_plain_text().as_deref(),
                Some("Context compacted\n\nVISIBLE-NOTE")
            );
            shell.toggle_disclosure();
            let expanded = selection_frame(&mut shell, threaded);
            let expected = "GROUPED 界e\u{301}";
            let (row, col) = selection_cell(&expanded, expected);
            for reverse in [false, true] {
                let end = col + visible_width(expected) as u16;
                let (from, to) = if reverse { (end, col) } else { (col, end) };
                shell.begin_transcript_selection(row, from, false);
                shell.extend_transcript_selection(row, to);
                assert_eq!(
                    shell.selected_plain_text().as_deref(),
                    Some(expected),
                    "threaded={threaded}, width={width}, reverse={reverse}"
                );
            }
            shell.select_all_transcript();
            let all = shell.selected_plain_text().unwrap();
            for disclosed in [
                "GROUPED-OUTPUT",
                "UNDISCLOSED-REASONING",
                "UNDISCLOSED-SUMMARY",
                "VISIBLE-NOTE",
            ] {
                assert!(all.contains(disclosed), "{all}");
            }
            assert!(
                !all.contains("PRIVATE-ARGS") && !all.contains("->"),
                "{all}"
            );
        }
    }
    // Input windows use the very same row budget for painting and copying.
    for name in ["bash", "codemode"] {
        let source = (0..20)
            .map(|row| format!("line{row:02} 界e\u{301}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = if name == "bash" {
            serde_json::json!({"command":source})
        } else {
            serde_json::json!({"code":source})
        };
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(48, 40);
        shell.state.borrow_mut().application_viewport_requested = true;
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Tool(Box::new(selection_panel(
                name,
                args,
                String::new(),
            ))));
        shell.select_all_transcript();
        let compact = shell.selected_plain_text().unwrap();
        assert!(compact.contains("line00"));
        assert!(
            !compact.contains("line19") && !compact.contains("Ctrl+O"),
            "{compact}"
        );
        shell.toggle_disclosure();
        shell.select_all_transcript();
        assert!(shell.selected_plain_text().unwrap().contains("line19"));
    }
}

#[test]
fn capture_and_diff_omission_metadata_never_become_tool_copy_source() {
    use super::super::InteractiveShell;
    for width in [48, 96] {
        for expanded in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(width, 40);
            shell.state.borrow_mut().application_viewport_requested = true;
            let bash = selection_panel("bash", serde_json::json!({"command":"printf fixture"}),
                "exit=0 duration=0.01s\nstdout:\nVALUE 界e\u{301}\ntruncated_stdout=true omitted_bytes:80\nfull_output_path=/PRIVATE-CAPTURE\nspill_expired=true".into());
            let mut edit = selection_panel(
                "edit",
                serde_json::json!({"path":"fixture.rs"}),
                "PRIVATE-RESULT".into(),
            );
            edit.diff = Some("--- a/fixture.rs\n+++ b/fixture.rs\n@@ -1 +1 @@\n-old\n+\tVALUE 界e\u{301}\n… 20 more lines\n".into());
            {
                let mut state = shell.state.borrow_mut();
                state.push_block(TranscriptBlock::Tool(Box::new(bash)));
                state.push_block(TranscriptBlock::Tool(Box::new(edit)));
            }
            if expanded {
                shell.toggle_disclosure();
            }
            let frame = selection_frame(&mut shell, false);
            shell.select_all_transcript();
            let all = shell.selected_plain_text().unwrap();
            assert!(
                all.contains("VALUE 界e\u{301}") && all.contains("+    VALUE"),
                "{all}"
            );
            for metadata in [
                "capture omitted",
                "unavailable",
                "PRIVATE-CAPTURE",
                "PRIVATE-RESULT",
                "20 more lines",
                "truncated_stdout",
                "duration=",
            ] {
                assert!(!all.contains(metadata), "{all}");
            }
            let (row, col) = selection_cell(&frame, "VALUE 界e\u{301}");
            shell.begin_transcript_selection(row, col, false);
            shell.extend_transcript_selection(row, col + visible_width("VALUE 界e\u{301}") as u16);
            assert_eq!(
                shell.selected_plain_text().as_deref(),
                Some("VALUE 界e\u{301}")
            );
        }
    }
}

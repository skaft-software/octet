//! Provider retry activity, uncertain usage, and the production status render loop. Separate
//! because they assert the retry state machine and its bounded repaint cadence.

use super::support::*;

use super::*;

#[test]
fn provider_retry_countdown_is_typed_and_stops_at_attempt_start_and_cancellation() {
    let mut shell = InteractiveShell::test_shell();
    let id = shell.begin_run("test");
    shell.on_run_event(id, &retry_event(1));
    assert!(
        strip_terminal_sequences(&render_shell(&shell.state.borrow(), 80).join("\n"))
            .contains("Retrying 1/3 in 5s")
    );
    {
        let state = shell.state.borrow();
        let TranscriptBlock::Reasoning(block) = &state.transcript[state.active_reasoning.unwrap()]
        else {
            panic!()
        };
        let retry = block.retry_activity.as_ref().unwrap();
        assert_eq!(retry.label_at(retry.observed_at), "Retrying 1/3 in 5s");
        assert_eq!(
            retry.label_at(retry.observed_at + Duration::from_secs(6)),
            "Retrying 1/3"
        );
    }
    shell.on_run_event(id, &AgentEvent::TurnStarted);
    assert!(
        !strip_terminal_sequences(&render_shell(&shell.state.borrow(), 80).join("\n"))
            .contains("Retrying")
    );
    shell.on_run_event(id, &retry_event(2));
    shell.set_run_preparing(id, "cancelling");
    assert!(
        !strip_terminal_sequences(&render_shell(&shell.state.borrow(), 80).join("\n"))
            .contains("Retrying")
    );
    shell.interrupt_run(id);
    shell.on_run_event(id, &retry_event(3));
    assert!(
        !strip_terminal_sequences(&render_shell(&shell.state.borrow(), 80).join("\n"))
            .contains("Retrying"),
        "stale retry must not reopen activity"
    );
}

#[test]
fn provider_retry_preserves_accepted_turn_output() {
    let mut shell = InteractiveShell::test_shell();
    let id = shell.begin_run("test");
    shell.on_run_event(
        id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "accepted reasoning".into(),
        },
    );
    shell.on_run_event(
        id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "accepted answer".into(),
        },
    );
    shell.on_run_event(
        id,
        &AgentEvent::TurnFinished {
            turn_cost: None,
            message: octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("accepted answer".into())],
                model: octet_ai::ModelId("test".into()),
                protocol: octet_ai::Protocol::OpenAiResponses,
            },
            stop_reason: octet_ai::StopReason::EndTurn,
            turn_usage: octet_ai::Usage::default(),
            usage: octet_ai::Usage::default(),
            session_cost_microdollars: None,
            run_cost_microdollars: 0,
        },
    );
    shell.on_run_event(
        id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "rejected second turn".into(),
        },
    );
    shell.on_run_event(id, &retry_event(1));
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("accepted answer"));
    assert!(snapshot.contains("accepted reasoning"));
    assert!(!snapshot.contains("rejected second turn"));
}

#[test]
fn provider_retry_activity_clears_on_output_and_compaction_and_survives_resize() {
    for output in [true, false] {
        let mut shell = InteractiveShell::test_shell();
        let id = shell.begin_run("test");
        shell.on_run_event(id, &retry_event(1));
        shell.set_size(46, 8);
        assert!(
            strip_terminal_sequences(&render_shell(&shell.state.borrow(), 46).join("\n"))
                .contains("Retrying 1/3")
        );
        if output {
            shell.on_run_event(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Reasoning,
                    text: "new reasoning".into(),
                },
            );
        } else {
            shell.on_run_event(
                id,
                &AgentEvent::CompactionStarted {
                    reason: octet_agent::CompactionReason::Overflow,
                },
            );
        }
        let state = shell.state.borrow();
        assert!(state.transcript.iter().all(|block| !matches!(block,
            TranscriptBlock::Reasoning(block) if block.retry_activity.is_some())));
    }
}

#[test]
fn provider_retry_network_wait_has_no_invented_retry_limit() {
    let now = Instant::now();
    let activity = assistant_block::RetryActivity {
        operation: None,
        attempt: 17,
        max_attempts: None,
        delay: Duration::from_secs(60),
        observed_at: now,
    };
    assert_eq!(
        activity.label_at(now),
        "Waiting for network · attempt 17 in 60s"
    );
    assert_eq!(
        activity.label_at(now + Duration::from_secs(61)),
        "Waiting for network · attempt 17"
    );
}

#[test]
fn provider_retry_network_wait_replaces_one_activity_without_rollback() {
    let mut shell = InteractiveShell::test_shell();
    let id = shell.begin_run("test");
    shell.on_run_event(
        id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Reasoning,
            text: "accepted reasoning".into(),
        },
    );
    shell.on_run_event(
        id,
        &AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text: "accepted answer".into(),
        },
    );
    shell.on_run_event(
        id,
        &AgentEvent::TurnFinished {
            turn_cost: None,
            message: octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("accepted answer".into())],
                model: octet_ai::ModelId("test".into()),
                protocol: octet_ai::Protocol::OpenAiResponses,
            },
            stop_reason: octet_ai::StopReason::EndTurn,
            turn_usage: octet_ai::Usage::default(),
            usage: octet_ai::Usage::default(),
            session_cost_microdollars: None,
            run_cost_microdollars: 0,
        },
    );
    shell.notice("independent network notice");
    for attempt in [1, 2] {
        shell.on_run_event(
            id,
            &AgentEvent::ProviderWaitingForNetwork {
                attempt,
                delay: Duration::from_secs(60),
                error: "diagnostic-only network cause".into(),
            },
        );
        let state = shell.state.borrow();
        let frame = strip_terminal_sequences(&render_shell(&state, 120).join("\n"));
        assert!(
            frame.contains(&format!("Waiting for network · attempt {attempt}")),
            "{frame}"
        );
        assert!(!frame.contains("diagnostic-only"));
        assert!(frame.contains("independent network notice"));
        assert!(frame.contains("accepted answer"));
        assert!(state.provisional_blocks.is_empty());
        assert_eq!(
            state
                .transcript
                .iter()
                .filter(|block| matches!(block,
            TranscriptBlock::Reasoning(block) if block.retry_activity.is_some()))
                .count(),
            1
        );
    }
    shell.on_run_event(id, &AgentEvent::TurnStarted);
    assert!(
        !strip_terminal_sequences(&render_shell(&shell.state.borrow(), 120).join("\n"))
            .contains("Waiting for network")
    );
}

#[test]
fn provider_retry_auxiliary_activity_preserves_answer_and_compaction_phase() {
    use octet_agent::ProviderOperation;
    for operation in [
        ProviderOperation::LocalCompaction,
        ProviderOperation::NativeCompaction,
        ProviderOperation::TerminalGate,
    ] {
        for max_attempts in [Some(3), None] {
            let mut shell = InteractiveShell::test_shell();
            let id = shell.begin_run("test");
            shell.on_run_event(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "retained main answer".into(),
                },
            );
            // This test also proves the auxiliary event does not invalidate
            // the still-provisional candidate while a terminal gate checks it.
            let owned = shell.state.borrow().provisional_blocks.clone();
            if operation != ProviderOperation::TerminalGate {
                let mut state = shell.state.borrow_mut();
                let index = state.active_reasoning.unwrap();
                let TranscriptBlock::Reasoning(block) = &mut state.transcript[index] else {
                    panic!()
                };
                block.reasoning_heading = Some("Compacting context".into());
                state.run_label = "compacting".into();
            }
            let phase = shell.state.borrow().run.current().unwrap().phase().clone();
            shell.on_run_event(
                id,
                &AgentEvent::ProviderOperationRetry {
                    operation,
                    attempt: 2,
                    max_attempts,
                    delay: Duration::from_secs(60),
                    error: "diagnostic-only auxiliary cause".into(),
                },
            );
            let state = shell.state.borrow();
            assert_eq!(state.provisional_blocks, owned);
            assert_eq!(state.run.current().unwrap().phase(), &phase);
            let frame = strip_terminal_sequences(&render_shell(&state, 160).join("\n"));
            assert!(frame.contains("retained main answer"));
            let label = match operation {
                ProviderOperation::LocalCompaction => "Local compaction",
                ProviderOperation::NativeCompaction => "Native compaction",
                ProviderOperation::TerminalGate => "Final-answer check",
                ProviderOperation::BranchSummary => "Branch summary",
            };
            assert!(frame.contains(label), "{frame}");
            assert!(
                frame.contains(if max_attempts.is_some() {
                    "Retrying 2/3"
                } else {
                    "Waiting for network"
                }),
                "{frame}"
            );
            assert!(!frame.contains("diagnostic-only"));
            assert!(state.has_active_status_timer());
            if operation != ProviderOperation::TerminalGate {
                assert_eq!(state.run_label, "compacting");
            }
        }
    }
}

#[test]
fn provider_usage_uncertain_survives_success_settlement_and_resume() {
    let mut shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().price_display = PriceDisplay::Priced;
    let id = shell.begin_run("test");
    shell.on_run_event(id, &AgentEvent::ProviderUsageUncertain);
    shell.on_run_event(
        id,
        &AgentEvent::TurnFinished {
            turn_cost: None,
            message: octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("accepted answer".into())],
                model: octet_ai::ModelId("test".into()),
                protocol: octet_ai::Protocol::OpenAiResponses,
            },
            stop_reason: octet_ai::StopReason::EndTurn,
            turn_usage: octet_ai::Usage {
                total_tokens: 15,
                ..Default::default()
            },
            usage: octet_ai::Usage {
                total_tokens: 15,
                ..Default::default()
            },
            session_cost_microdollars: Some(4_200),
            run_cost_microdollars: 4_200,
        },
    );
    shell.on_run_event(
        id,
        &AgentEvent::RunFinished {
            head: octet_agent::EntryId("accepted".into()),
            reason: octet_agent::FinishReason::Completed,
        },
    );
    let footer = plain_footer(&shell, 120, Instant::now());
    assert!(footer.contains("$0.0042"), "{footer}");
    assert!(
        !footer.contains("subtotal") && !footer.contains('?'),
        "{footer}"
    );
    assert!(shell.state.borrow().usage_uncertain);
    let telemetry = status_telemetry::status_telemetry(&shell.state.borrow(), Instant::now());
    assert!(telemetry.contains("Session cost   $0.004200"));
    assert!(!telemetry.contains("subtotal") && !telemetry.contains("unknown"));
    assert!(!telemetry.contains("(exact)"));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("uncertain.jsonl");
    let mut session = octet_agent::Session::create(&path).unwrap();
    session
        .record_usage_uncertainty(
            octet_ai::EndpointId("codex".into()),
            octet_ai::ModelId("test".into()),
            "assistant_turn",
        )
        .unwrap();
    drop(session);
    shell
        .hydrate(&octet_agent::Session::open(&path).unwrap())
        .unwrap();
    assert!(shell.state.borrow().usage_uncertain);
    let resumed_footer = plain_footer(&shell, 120, Instant::now());
    assert!(
        !resumed_footer.contains("usage/cost unknown"),
        "{resumed_footer}"
    );
    assert!(shell.state.borrow().usage_uncertain);
    shell.begin_run("test");
    assert!(
        shell.state.borrow().usage_uncertain,
        "next run cannot reset session uncertainty"
    );
    let fresh = octet_agent::Session::create(directory.path().join("fresh.jsonl")).unwrap();
    shell.hydrate(&fresh).unwrap();
    assert!(
        !shell.state.borrow().usage_uncertain,
        "replacing the session clears only the old session's uncertainty"
    );
}

#[test]
fn provider_usage_uncertain_is_retained_with_configured_zero_price_display() {
    let mut shell = InteractiveShell::test_shell();
    shell.state.borrow_mut().price_display = PriceDisplay::ExplicitZero;
    let id = shell.begin_run("test");
    shell.on_run_event(id, &AgentEvent::ProviderUsageUncertain);
    let footer = plain_footer(&shell, 120, Instant::now());
    assert!(footer.contains("$0"), "{footer}");
    assert!(!footer.contains("unknown"), "{footer}");
    let telemetry = status_telemetry::status_telemetry(&shell.state.borrow(), Instant::now());
    assert!(
        telemetry.contains("$0 (configured zero-priced)"),
        "{telemetry}"
    );
    assert!(shell.state.borrow().usage_uncertain);
    assert_eq!(shell.state.borrow().session_cost_microdollars, None);
    assert!(!shell.state.borrow().run_cost_available);
    for width in [46, 80, 120] {
        let frame = render_shell(&shell.state.borrow(), width);
        assert!(frame
            .iter()
            .all(|line| visible_width(line) <= usize::from(width)));
    }
}

#[test]
fn renderer_stop_flushes_unpainted_notices_even_when_it_preempts_render() {
    for queued_render in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.tui.take().unwrap().stop();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let terminal = EmulatedTerminal {
            size: shell.size.clone(),
            bytes: bytes.clone(),
            synchronized_output: true,
            status_frames: None,
        };
        let (tx, rx) = mpsc::channel();
        renderer_runtime::render_loop_with_terminal(
            terminal,
            shell.state.clone(),
            shell.size.clone(),
            rx,
            renderer_runtime::RenderLoopOptions::default(),
            |state, _| {
                // The idle poll runs after the initial frame. Install the final
                // semantic notice and Stop before another frame can be painted.
                state
                    .borrow_mut()
                    .push_block(TranscriptBlock::Notice("FINAL-REEXEC-NOTICE".to_owned()));
                if queued_render {
                    tx.send(RenderCommand::Render).unwrap();
                }
                tx.send(RenderCommand::Stop).unwrap();
                false
            },
        );
        let output = bytes.lock().unwrap();
        let plain = strip_terminal_sequences(&String::from_utf8_lossy(&output));
        assert_eq!(plain.matches("FINAL-REEXEC-NOTICE").count(), 1, "{plain}");
    }
}

#[test]
fn status_render_loop_skips_missed_phases_after_expensive_frames() {
    let StatusRenderLoop {
        mut shell,
        frames,
        delay,
        ..
    } = status_render_loop_shell(crate::tui::theme::test_theme());
    let first = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(first.1, 0);
    // Delay real terminal frame completion, not a direct animation-method call.
    // After each delay the next single frame must select the current phase,
    // rather than slowing to one cell for every expensive render.
    delay.store(240, std::sync::atomic::Ordering::Relaxed);
    let mut previous = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    for _ in 0..3 {
        let next = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            next.1 - previous.1 >= 3,
            "missed phases were lost: {previous:?} -> {next:?}"
        );
        let elapsed_ticks = next.0.duration_since(previous.0).as_millis() / 80;
        assert!(u128::from((next.1 - previous.1) as u64).abs_diff(elapsed_ticks) <= 1);
        previous = next;
    }
    delay.store(0, std::sync::atomic::Ordering::Relaxed);
    shell.stop_renderer();
}

#[test]
fn status_render_loop_working_is_animated_without_terminal_floods() {
    let StatusRenderLoop {
        mut shell,
        frames,
        bytes,
        ..
    } = status_render_loop_shell(crate::tui::theme::test_theme());
    let first = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(first.1, 0);
    // Missed 80 ms deadlines skip frames, so a shared runner need not paint
    // every frame in 2050 ms. Await the sweep and the seven-letter label's ANSI palette coverage,
    // with one finite liveness deadline, rather than asserting OS throughput.
    let deadline = first.0 + Duration::from_secs(10);
    let mut sample = first;
    let mut samples = 0;
    let mut consumed = 0;
    let mut parser = vt100::Parser::new(24, 80, 100);
    let mut palettes = Vec::new();
    loop {
        let (at, phase, end) = sample;
        let elapsed_ticks = at.duration_since(first.0).as_millis() / 80;
        assert!(
            (phase as u128).abs_diff(elapsed_ticks) <= 1,
            "phase {phase} at {elapsed_ticks} elapsed ticks"
        );
        // Upper-bound actual writes by elapsed cadence, not a fixed sleep's
        // expected count. A late renderer must not replay missed frames.
        assert!(samples <= elapsed_ticks + 1, "frame flood: {samples}");
        assert!(end < 100_000, "terminal byte flood: {end}");
        let frame = bytes.lock().unwrap()[consumed..end].to_vec();
        assert!(!frame.windows(4).any(|part| part == b"\x1b[3J"));
        parser.process(&frame);
        consumed = end;
        for (row, line) in parser.screen().rows(0, 80).enumerate() {
            if let Some(column) = line.find("Working") {
                let palette = (column..column + "Working".len())
                    .map(|column| {
                        parser
                            .screen()
                            .cell(row as u16, column as u16)
                            .unwrap()
                            .fgcolor()
                    })
                    .collect::<Vec<_>>();
                if !palettes.contains(&palette) {
                    palettes.push(palette);
                }
            }
        }
        if phase >= 24 && palettes.len() >= 12 {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "incomplete sweep: phase {phase}, {samples} frames, {} palettes",
            palettes.len()
        );
        sample = frames.recv_timeout(remaining).unwrap_or_else(|error| {
            panic!(
                "waiting for sweep: {error}; phase {phase}, {samples} frames, {} palettes",
                palettes.len()
            )
        });
        samples += 1;
    }
    eprintln!(
        "status-loop working: {samples} frames, {} palettes, phase {} in {:?}",
        palettes.len(),
        sample.1,
        sample.0.duration_since(first.0)
    );
    shell.state.borrow_mut().close_activity_status("Working");
    shell.render();
    // A queued or in-flight animation frame can precede the settlement paint.
    // Fence quiescence on the actual screen without Working, not the next write.
    let settlement_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let (_, _, end) = frames
            .recv_timeout(settlement_deadline.saturating_duration_since(Instant::now()))
            .expect("Working settlement was not painted");
        parser.process(&bytes.lock().unwrap()[consumed..end]);
        consumed = end;
        if !parser.screen().contents().contains("Working") {
            break;
        }
    }
    assert!(matches!(
        frames.recv_timeout(Duration::from_millis(250)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    shell.stop_renderer();
}

#[test]
fn status_render_loop_disabled_motion_stays_static() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    let mut reduced = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    reduced.animation = false;
    for capabilities in [
        reduced,
        TerminalCapabilities::test(true, true, ColorDepth::None),
        TerminalCapabilities::test(false, true, ColorDepth::TrueColor),
    ] {
        let StatusRenderLoop {
            mut shell, frames, ..
        } = status_render_loop_shell(crate::tui::theme::test_theme_with(capabilities));
        frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            frames.recv_timeout(Duration::from_millis(250)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(shell.state.borrow().status_shimmer_frame, 0);
        shell.stop_renderer();
    }
}

#[test]
fn status_render_loop_busy_notifications_keep_phase_and_stop_responsive() {
    let StatusRenderLoop {
        mut shell,
        frames,
        bytes,
        ..
    } = status_render_loop_shell(crate::tui::theme::test_theme());
    frames.recv_timeout(Duration::from_secs(2)).unwrap();
    let sender = shell.render_tx.lock().unwrap().as_ref().unwrap().clone();
    thread::scope(|scope| {
        scope.spawn(move || {
            let until = Instant::now() + Duration::from_secs(3);
            while Instant::now() < until {
                if matches!(
                    sender.try_send(RenderCommand::Render),
                    Err(mpsc::TrySendError::Disconnected(_))
                ) {
                    break;
                }
                thread::yield_now();
            }
        });
        let first = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        thread::sleep(Duration::from_millis(700));
        let samples = frames.try_iter().collect::<Vec<_>>();
        let last = samples
            .last()
            .expect("busy queue must not starve animation");
        assert!(
            (5..=50).contains(&samples.len()),
            "frames: {}",
            samples.len()
        );
        let ticks = last.0.duration_since(first.0).as_millis() / 80;
        assert!(((last.1 - first.1) as u128).abs_diff(ticks) <= 1);
        assert!(last.1 >= first.1 + 6);
        // Independent observer: a mutex/channel stall cannot block its timeout.
        // This is the same Stop-send/join sequence as stop_renderer, without
        // moving the non-Send inline-test TUI to the observer thread.
        let stop_tx = shell.render_tx.lock().unwrap().take().unwrap();
        let render_thread = shell.render_thread.take().unwrap();
        let (done, stopped) = mpsc::channel();
        let start = Instant::now();
        scope.spawn(move || {
            stop_tx.send(RenderCommand::Stop).unwrap();
            render_thread.join().unwrap();
            done.send(()).unwrap();
        });
        stopped
            .recv_timeout(Duration::from_millis(500))
            .expect("Stop must preempt busy coalescing");
        eprintln!(
            "status-loop busy: {} frames in 700ms, Stop joined in {:?}",
            samples.len(),
            start.elapsed()
        );
    });
    assert!(bytes.lock().unwrap().len() < 100_000);
}

#[test]
fn status_render_loop_retry_compaction_and_cancellation_transitions() {
    let StatusRenderLoop {
        mut shell,
        frames,
        bytes,
        ..
    } = status_render_loop_shell(crate::tui::theme::test_theme());
    let mut parser = vt100::Parser::new(24, 80, 100);
    let mut consumed = 0;
    let mut await_label = |label: &str| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let (at, phase, _) = frames
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "waiting for {label}: {error}; screen: {}",
                        parser.screen().contents()
                    )
                });
            let output = bytes.lock().unwrap();
            parser.process(&output[consumed..]);
            consumed = output.len();
            if parser.screen().contents().contains(label) {
                return (at, phase);
            }
        }
    };
    await_label("Working");
    shell.state.borrow_mut().close_activity_status("Working");
    let id = shell.begin_run("openai");
    shell.on_run_event(id, &retry_event(1));
    shell.render();
    let first = await_label("Retrying 1/3");
    let second = await_label("Retrying 1/3");
    assert_eq!(second.1, first.1, "retry/compaction timers do not shimmer");
    shell.on_run_event(
        id,
        &AgentEvent::CompactionStarted {
            reason: octet_agent::CompactionReason::Overflow,
        },
    );
    shell.render();
    let first = await_label("Compacting context");
    let second = await_label("Compacting context");
    assert_eq!(second.1, first.1, "retry/compaction timers do not shimmer");
    shell.on_run_event(
        id,
        &AgentEvent::CompactionFinished {
            reason: octet_agent::CompactionReason::Overflow,
            result: Ok(octet_agent::CompactionInfo {
                kind: octet_agent::CompactionKind::Local,
                summary: "synthetic compaction outcome".into(),
                first_kept: octet_agent::EntryId("kept".into()),
                usage: octet_ai::Usage::default(),
                elapsed: Duration::ZERO,
                cost_microdollars: None,
            }),
        },
    );
    shell.render();
    await_label("Working");
    shell.set_run_preparing(id, "cancelling");
    shell.interrupt_run(id);
    shell.render();
    await_label("interrupted");
    assert!(!shell.state.borrow().has_active_status_shimmer());
    assert!(!parser.screen().contents().contains("Compacting context"));
    assert!(!parser.screen().contents().contains("Retrying"));
    shell.stop_renderer();
}

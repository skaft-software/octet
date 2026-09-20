use std::time::{Duration, Instant};

use octet_ai::Usage;

use super::terminal_text::sanitize_for_terminal;
use super::{OctetTheme, PriceDisplay, ShellState};

/// Calculate a nonzero output-generation rate from a token count and measured
/// generation interval. Completed turns pass provider-reported tokens; live
/// rendering passes the explicitly marked character-based estimate.
pub(super) fn output_tokens_per_second(output_tokens: u64, elapsed: Duration) -> Option<f64> {
    (output_tokens > 0 && !elapsed.is_zero())
        .then(|| output_tokens as f64 / elapsed.as_secs_f64())
        .filter(|rate| rate.is_finite())
}

pub(super) fn usage_cache_hit_rate_basis_points(usage: Usage) -> Option<u16> {
    let prompt_tokens = usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens);
    if prompt_tokens == 0 || (usage.cache_read_tokens == 0 && usage.cache_write_tokens == 0) {
        return None;
    }
    Some(((u128::from(usage.cache_read_tokens) * 10_000) / u128::from(prompt_tokens)) as u16)
}

fn status_dollars(microdollars: u64) -> String {
    format!("${:.6}", microdollars as f64 / 1_000_000.0)
}

pub(super) fn status_telemetry(state: &ShellState, now: Instant) -> String {
    let mut lines = vec!["Telemetry".to_owned()];
    if let Some(usage) = state.last_turn_usage {
        lines.extend([
            // One plain label in every case: the provider API is the source of
            // truth. Interrupted usage stays recorded in `usage_uncertain` and
            // is surfaced on the non-TUI channels, never annotated here.
            "Usage source   provider-reported".to_owned(),
            format!("Input tokens   {}", usage.input_tokens),
            format!("Cache read     {}", usage.cache_read_tokens),
            format!("Cache write    {}", usage.cache_write_tokens),
            format!("Output tokens  {}", usage.output_tokens),
            format!("Reasoning      {}", usage.reasoning_tokens),
            format!("Total tokens   {}", usage.total_tokens),
        ]);
    } else if let Some(tokens) = state.live_generated_tokens() {
        lines.push(format!("Output tokens  ~{tokens} (stream estimate)"));
        lines.push("Usage source   awaiting provider report".to_owned());
    } else {
        lines.push("Usage source   unavailable (no completed model turn)".to_owned());
    }

    // Plain dollars, nothing else: a coding agent provides an estimate and the
    // provider API is the source of truth. Interrupted-usage uncertainty is
    // still recorded durably in `usage_uncertain` and still surfaced on the
    // non-TUI channels; it is never rendered here as `+ unknown` or `~`.
    match state.price_display {
        PriceDisplay::Unknown => {
            lines.push("Turn cost      unavailable (pricing not configured)".to_owned());
            lines.push("Session cost   unavailable (pricing not configured)".to_owned());
        }
        PriceDisplay::ExplicitZero => {
            lines.push("Turn cost      $0 (configured zero-priced)".to_owned());
            lines.push("Session cost   $0 (configured zero-priced)".to_owned());
        }
        PriceDisplay::Priced => {
            if state.run_cost_available {
                lines.push(format!(
                    "Turn cost      {}",
                    status_dollars(state.run_cost_microdollars)
                ));
            } else {
                lines.push("Turn cost      unavailable (no durable completed run)".to_owned());
            }
            lines.push(match state.session_cost_microdollars {
                Some(cost) => format!("Session cost   {}", status_dollars(cost)),
                None => "Session cost   awaiting first usage report".to_owned(),
            });
        }
    }

    if let (Some(rate), Some(tokens), Some(elapsed)) = (
        state.last_turn_tokens_per_second,
        state.last_turn_generated_tokens,
        state.last_turn_generation_elapsed,
    ) {
        lines.push(format!(
            "Throughput     {rate:.1} tok/s final ({tokens} reported tokens / {:.2}s measured)",
            elapsed.as_secs_f64()
        ));
    } else if let Some(started) = state.turn_generation_started_at {
        lines.push(format!(
            "Throughput     awaiting turn completion ({:.2}s generation in progress)",
            now.saturating_duration_since(started).as_secs_f64()
        ));
    } else {
        lines.push("Throughput     unavailable".to_owned());
    }
    lines.join("\n")
}

pub(super) fn styled_status_text(theme: &OctetTheme, text: &str) -> String {
    let safe = sanitize_for_terminal(text);
    let mut metadata = true;
    safe.lines()
        .map(|line| {
            if line.is_empty() {
                metadata = false;
                return String::new();
            }
            if !metadata {
                return line.to_owned();
            }
            let Some(separator) = line.find("  ") else {
                return line.to_owned();
            };
            let label = &line[..separator];
            let spacing_and_value = &line[separator..];
            let spacing = spacing_and_value
                .chars()
                .take_while(|character| character.is_whitespace())
                .collect::<String>();
            let value = &spacing_and_value[spacing.len()..];
            let value = if label == "Model" {
                theme.bold(&theme.fg("model_accent", value))
            } else {
                value.to_owned()
            };
            format!("{}{}{}", theme.fg("model_accent", label), spacing, value)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use futures_util::StreamExt as _;

    #[test]
    fn output_token_rate_uses_authoritative_usage_and_generation_elapsed_time() {
        assert_eq!(
            output_tokens_per_second(120, Duration::from_secs(2)),
            Some(60.0)
        );
        assert!(output_tokens_per_second(1, Duration::from_millis(250))
            .is_some_and(|rate| (rate - 4.0).abs() < f64::EPSILON));
        assert_eq!(output_tokens_per_second(0, Duration::from_secs(1)), None);
        assert_eq!(output_tokens_per_second(1, Duration::ZERO), None);
    }

    /// A priced session whose latest turn is settled, with the durable
    /// interrupted-usage flag under test.
    fn priced_state(usage_uncertain: bool) -> ShellState {
        ShellState {
            usage_uncertain,
            price_display: PriceDisplay::Priced,
            run_cost_available: true,
            run_cost_microdollars: 78_900,
            session_cost_microdollars: Some(2_410_000),
            last_turn_usage: Some(Usage {
                input_tokens: 1_000,
                output_tokens: 250,
                total_tokens: 1_250,
                ..Usage::default()
            }),
            ..ShellState::default()
        }
    }

    #[test]
    fn cost_lines_are_plain_dollars_even_when_usage_is_uncertain() {
        for usage_uncertain in [true, false] {
            let state = priced_state(usage_uncertain);
            let telemetry = status_telemetry(&state, Instant::now());
            assert!(
                telemetry.contains("Turn cost      $0.078900"),
                "{telemetry:?}"
            );
            assert!(
                telemetry.contains("Session cost   $2.410000"),
                "{telemetry:?}"
            );
            // Only the dollar figures: no subtotal wording, no `+ unknown`,
            // no `~` approximation and no `?` uncertainty marker.
            for forbidden in ["subtotal", "+", "~", "?", "unknown", "(exact)"] {
                assert!(
                    !telemetry.contains(forbidden),
                    "{forbidden:?} leaked into {telemetry:?}"
                );
            }
            // The usage-source label is the same plain fact in both cases.
            assert!(
                telemetry
                    .lines()
                    .any(|line| line == "Usage source   provider-reported"),
                "{telemetry:?}"
            );
            // Uncertainty is a durable state fact, not a rendering claim.
            assert_eq!(
                state.usage_uncertain, usage_uncertain,
                "cost rendering must not clear the recorded uncertainty"
            );
        }
    }

    /// Every priced rendering path stays a plain dollar figure — including the
    /// reported case where the session carries durable interrupted-usage records
    /// — while the token stream estimate keeps its own `~`, which is not cost.
    #[test]
    fn cost_lines_never_carry_uncertainty_markers_in_any_cost_state() {
        for usage_uncertain in [false, true] {
            for price_display in [
                PriceDisplay::Priced,
                PriceDisplay::ExplicitZero,
                PriceDisplay::Unknown,
            ] {
                for (run_cost_available, session_cost_microdollars) in [
                    (true, Some(2_410_000u64)),
                    (true, None),
                    (false, Some(2_410_000)),
                    (false, None),
                ] {
                    let mut state = priced_state(usage_uncertain);
                    state.price_display = price_display;
                    state.run_cost_available = run_cost_available;
                    state.session_cost_microdollars = session_cost_microdollars;
                    let telemetry = status_telemetry(&state, Instant::now());
                    assert_cost_lines_are_plain_dollars(&telemetry);
                    assert_eq!(
                        state.usage_uncertain, usage_uncertain,
                        "rendering must never clear the durable uncertainty flag"
                    );
                }
            }
        }

        // A live turn has only a streaming token estimate; the cost lines still
        // show a plain dollar figure or an honest absence, never `~` or `?`.
        let mut live = priced_state(true);
        live.last_turn_usage = None;
        live.run_cost_available = false;
        live.session_cost_microdollars = None;
        live.turn_generation_started_at = Some(Instant::now());
        live.turn_streamed_output_bytes = 480;
        let telemetry = status_telemetry(&live, Instant::now());
        assert!(
            telemetry.contains("Output tokens  ~120 (stream estimate)"),
            "{telemetry:?}"
        );
        assert_cost_lines_are_plain_dollars(&telemetry);
    }

    /// The cost area is only the two labelled cost lines: no `+`, `~`, `?`,
    /// "subtotal" or "unknown" marker may ever appear there.
    fn assert_cost_lines_are_plain_dollars(telemetry: &str) {
        let cost_lines = telemetry
            .lines()
            .filter(|line| line.starts_with("Turn cost") || line.starts_with("Session cost"))
            .collect::<Vec<_>>();
        assert_eq!(cost_lines.len(), 2, "{telemetry:?}");
        for line in cost_lines {
            for forbidden in ["subtotal", "+", "~", "?", "unknown"] {
                assert!(
                    !line.contains(forbidden),
                    "{forbidden:?} leaked into {line:?} ({telemetry:?})"
                );
            }
        }
    }

    #[test]
    fn honest_absence_cost_paths_survive_plain_dollar_rendering() {
        // Pricing not configured stays honest even with interrupted usage.
        let mut state = priced_state(true);
        state.price_display = PriceDisplay::Unknown;
        state.last_turn_usage = None;
        let telemetry = status_telemetry(&state, Instant::now());
        assert!(
            telemetry.contains("Turn cost      unavailable (pricing not configured)"),
            "{telemetry:?}"
        );
        assert!(
            telemetry.contains("Session cost   unavailable (pricing not configured)"),
            "{telemetry:?}"
        );
        assert!(state.usage_uncertain, "uncertainty still recorded");
        assert!(!telemetry.contains("$0.000"), "{telemetry:?}");

        // An explicit zero price is a configured zero, never a guess.
        let mut state = priced_state(true);
        state.price_display = PriceDisplay::ExplicitZero;
        let telemetry = status_telemetry(&state, Instant::now());
        assert!(
            telemetry.contains("Turn cost      $0 (configured zero-priced)"),
            "{telemetry:?}"
        );
        assert!(
            telemetry.contains("Session cost   $0 (configured zero-priced)"),
            "{telemetry:?}"
        );

        // A priced session with no durable completed run reports absence.
        let mut state = priced_state(true);
        state.run_cost_available = false;
        state.session_cost_microdollars = None;
        let telemetry = status_telemetry(&state, Instant::now());
        assert!(
            telemetry.contains("Turn cost      unavailable (no durable completed run)"),
            "{telemetry:?}"
        );
        assert!(
            telemetry.contains("Session cost   awaiting first usage report"),
            "{telemetry:?}"
        );
        assert!(!telemetry.contains("subtotal"), "{telemetry:?}");
        assert!(!telemetry.contains('?'), "{telemetry:?}");
    }

    /// The startup phase is silent and the ready frame is atomic. This module
    /// can render the retained shell frame, so the gate is asserted on the
    /// painted frame; the phase itself runs through the real silent helper and
    /// the real `TerminalInput` owner used by `modes::interactive`.
    #[tokio::test]
    async fn silent_startup_paints_a_blank_typeable_composer_and_one_ready_frame() {
        use crate::tui::terminal::TerminalInput;
        use crate::tui::view::renderer_runtime::ShellComponent;
        use crate::tui::view::InteractiveShell;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        use sexy_tui_rs::{strip_terminal_sequences, Component, CURSOR_MARKER};

        fn plain(lines: &[String]) -> String {
            strip_terminal_sequences(&lines.join("\n"))
        }

        let mut shell = InteractiveShell::test_shell();
        shell.set_size(96, 18);
        shell.state.borrow_mut().startup_pending = true;
        let component = std::rc::Rc::new(ShellComponent::new(shell.state.clone(), false));

        // Every recorded paint happens while the startup phase is unfinished:
        // the phase resolves only after the input owner has processed every
        // queued keystroke, mirroring a real load.
        let recorded = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let mut done_tx = Some(done_tx);
        let mut captured_last = false;
        let keys = [
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            ))),
            Ok(Event::Paste(" draft during startup ".into())),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('z'),
                KeyModifiers::NONE,
            ))),
        ];
        let observer = component.clone();
        let sink = recorded.clone();
        let observer_at_end = component.clone();
        let sink_at_end = recorded.clone();
        let source = tokio_stream::iter(keys)
            .inspect(move |_| {
                sink.borrow_mut().push(observer.render(96).join("\n"));
            })
            .chain(futures_util::stream::poll_fn(move |_| {
                if !captured_last {
                    captured_last = true;
                    sink_at_end
                        .borrow_mut()
                        .push(observer_at_end.render(96).join("\n"));
                }
                if let Some(done_tx) = done_tx.take() {
                    let _ = done_tx.send(());
                }
                std::task::Poll::Ready(None)
            }));
        let mut input = TerminalInput::from_stream(source);
        crate::modes::interactive::run_blocking_startup_lifecycle(
            &mut shell,
            &mut input,
            "startup build",
            move || {
                done_rx.recv()?;
                Ok(())
            },
        )
        .await
        .expect("the silent startup phase completes");

        // 1. No startup chatter before readiness: no phase label, no notice, no
        //    bootstrap trace line, no branding.
        let frames = recorded.borrow().clone();
        assert!(!frames.is_empty());
        for frame in &frames {
            assert!(
                frame.contains(CURSOR_MARKER),
                "startup lost its cursor: {frame:?}"
            );
            for forbidden in [
                "starting extensions",
                "extensions",
                "discovering models",
                "model discovery",
                "startup build",
                "opening session",
                "signing in",
                "octet-startup",
                "octet",
                "codex",
                "Codex context",
                "notice",
            ] {
                assert!(
                    !frame.contains(forbidden),
                    "{forbidden:?} was painted during startup: {frame:?}"
                );
            }
        }

        // Silent startup is unbranded, not invisible: input remains typeable
        // and the retained draft is painted before extension discovery ends.
        assert_eq!(shell.pending(), "x draft during startup z");
        let last = frames.last().expect("a frame was painted");
        assert!(last.contains("x draft during startup z"), "{last:?}");
        assert!(last.contains(CURSOR_MARKER), "{last:?}");

        // 3. The frame assertion is sensitive: a lifecycle label really would
        //    paint, and while such a label is present the composer paints the
        //    draft with a live cursor.
        let mut control = InteractiveShell::test_shell();
        control.set_size(96, 18);
        control.state.borrow_mut().startup_pending = true;
        control
            .state
            .borrow_mut()
            .editor
            .set_text("x draft during startup z");
        control.set_run_label("discovering models…");
        let control_lines = ShellComponent::new(control.state.clone(), false).render(96);
        let control_frame = plain(&control_lines);
        assert!(
            control_frame.contains("discovering models"),
            "{control_frame:?}"
        );
        assert!(
            control_frame.contains("x draft during startup z"),
            "{control_frame:?}"
        );
        assert!(
            control_lines
                .iter()
                .any(|line| line.contains(CURSOR_MARKER)),
            "{control_frame:?}"
        );

        // 4. Readiness paints one atomic frame: identity, workspace, retained
        //    notice appear together with the already visible draft. Branding was not
        //    visible in any earlier paint.
        shell.set_identity("cerebras", "cerebras/gemma-4-31b", "off");
        shell.set_workspace(std::path::PathBuf::from("/startup-fixture/workspace"));
        shell.notice("read-only onboarding notice");
        shell.finish_startup();
        let ready_lines = component.render(96);
        let ready = plain(&ready_lines);
        assert!(
            ready_lines.iter().any(|line| line.contains(CURSOR_MARKER)),
            "the ready frame keeps a live composer cursor: {ready:?}"
        );
        for expected in [
            "cerebras/gemma-4-31b",
            "/startup-fixture/workspace",
            "read-only onboarding notice",
            "x draft during startup z",
        ] {
            assert!(
                ready.contains(expected),
                "{expected:?} missing from {ready:?}"
            );
        }
        for frame in &frames {
            for absent in [
                "cerebras/gemma-4-31b",
                "/startup-fixture/workspace",
                "read-only onboarding notice",
            ] {
                assert!(
                    !frame.contains(absent),
                    "pre-ready frame already painted {absent:?}: {frame:?}"
                );
            }
        }
    }

    #[test]
    fn status_metadata_uses_the_model_accent_but_no_color_stays_plain() {
        let mut theme = crate::tui::theme::test_theme();
        crate::tui::theme::apply_model_lab(&mut theme, crate::tui::theme::ModelLab::Anthropic);
        let styled = styled_status_text(
            &theme,
            "Provider       anthropic\nModel          claude\nReasoning      high\n\nSecurity model: trusted local agent",
        );
        assert!(styled.contains("38;2;169;99;76"), "{styled:?}");
        assert!(styled.contains("Model"));
        assert!(styled.contains("claude"));

        let mut plain = crate::tui::theme::test_theme_with(TerminalCapabilities::test(
            true,
            true,
            ColorDepth::None,
        ));
        crate::tui::theme::apply_model_lab(&mut plain, crate::tui::theme::ModelLab::Anthropic);
        let plain = styled_status_text(&plain, "Model          claude");
        assert_eq!(plain, "Model          claude");
        assert!(!plain.contains('\x1b'));
    }
}

/// Chrome for extension slash-command output. The body is sanitized here, then
/// framed with a heading rule and light per-line styling so long extension
/// reports stay scannable: `label:` prefixes read as headings, `-` bullets get
/// a quiet marker, and `·` separators stay dim.
pub(super) fn styled_extension_output(theme: &OctetTheme, command: &str, text: &str) -> String {
    let safe = sanitize_for_terminal(text);
    let rule_width = 28;
    let rule = theme.fg("muted", &theme.glyph("horizontal").repeat(rule_width));
    let heading = format!(
        "{} {}",
        theme.settled_event_dot("neutral", if theme.unicode() { "•" } else { "*" }),
        theme.bold(&theme.fg("foreground", &format!("/{command}")))
    );
    let mut lines = vec![rule.clone(), heading, rule.clone()];
    for line in safe.lines() {
        let styled_line = if let Some((label, rest)) = line.split_once(':') {
            if !label.is_empty()
                && label.len() <= 32
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_')
                && !rest.contains(':')
            {
                format!("{}{}{}", theme.fg("model_accent", label), ":", rest)
            } else {
                line.to_owned()
            }
        } else if let Some(rest) = line.strip_prefix("- ") {
            format!("{} {}", theme.fg("muted", "-"), rest)
        } else {
            line.to_owned()
        };
        lines.push(styled_line);
    }
    lines.push(rule);
    lines.join("\n")
}

#[cfg(test)]
mod extension_output_tests {
    use super::*;

    #[test]
    fn styled_extension_output_frames_and_labels_the_body() {
        let theme = crate::tui::theme::test_theme();
        let styled = styled_extension_output(
            &theme,
            "web-search",
            "provider: brave\n- result one\nplain line\nsome:thing: odd",
        );
        let plain = sanitize_for_terminal(&styled);
        assert!(plain.contains("/web-search"));
        assert!(plain.contains("provider: brave"));
        assert!(plain.contains("- result one"));
        // Styling must survive as trusted ANSI in the overlay text.
        assert!(styled.contains('\x1b'));
        // The body itself was sanitized; nothing raw leaks through.
        assert!(!styled.contains("\u{7}"));
    }

    #[test]
    fn styled_extension_output_is_plain_when_color_depth_is_none() {
        let theme =
            crate::tui::theme::test_theme_with(crate::tui::terminal::TerminalCapabilities::test(
                true,
                true,
                crate::tui::terminal::ColorDepth::None,
            ));
        let styled = styled_extension_output(&theme, "extensions", "no extensions configured");
        assert!(!styled.contains('\x1b'), "{styled:?}");
        assert!(styled.contains("/extensions"));
    }
}

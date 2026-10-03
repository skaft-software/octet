//! Tests for the picker state machine in this module: secret input capture,
//! model and file pickers, and the shell/panel wiring they drive.
//!
//! Split out of pickers.rs so the picker lifecycle code stays readable on its
//! own; the panel, draft-buffer and stream bookkeeping that makes a picker
//! interactive is the part worth reading, not the assertions over it.

use super::*;
use crossterm::event::{KeyEvent, KeyModifiers};
use tokio_stream::wrappers::ReceiverStream;

#[tokio::test]
async fn secret_typing_and_host_enter_ignore_remapped_or_disabled_picker_keys() {
    use crate::tui::keymap::keybindings::KeybindingsManager;
    use std::collections::BTreeMap;
    for confirm in [vec!["y".into()], Vec::new()] {
        let mut shell = InteractiveShell::test_shell();
        shell.test_set_keybindings(KeybindingsManager::with_platform(
            "linux",
            false,
            BTreeMap::from([
                ("tui.select.confirm".into(), confirm),
                ("tui.select.cancel".into(), vec!["n".into()]),
                ("tui.select.down".into(), vec!["j".into()]),
            ]),
        ));
        shell.prefill_editor("parent draft".into());
        let request = ExtensionInputRequest {
            parent_request_id: 0,
            prompt: "Fixture secret".into(),
            secret: true,
        };
        let mut input = futures_util::stream::iter([
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            ))),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::NONE,
            ))),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
            ))),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
        ]);
        assert_eq!(
            extension_input_picker(&mut shell, &mut input, &request)
                .await
                .unwrap()
                .as_deref(),
            Some("ynj")
        );
        assert_eq!(shell.pending(), "parent draft");
    }
}

#[tokio::test]
async fn secret_input_paste_never_enters_the_composer_or_transcript() {
    let mut shell = InteractiveShell::test_shell();
    shell.extension_set_editor("draft kept intact".into());
    let request = ExtensionInputRequest {
        parent_request_id: 0,
        prompt: "API key (input hidden)".into(),
        secret: true,
    };
    let mut input = futures_util::stream::iter([
        Ok(Event::Paste("synthetic-private-key\r\n".into())),
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
    ]);
    let answer = extension_input_picker(&mut shell, &mut input, &request)
        .await
        .unwrap();
    assert_eq!(answer.as_deref(), Some("synthetic-private-key"));
    assert_eq!(shell.pending(), "draft kept intact");
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(!frame.contains("synthetic-private-key"));
    assert!(!frame.contains("API key (input hidden)"));
}

#[tokio::test]
async fn secret_input_cancel_and_input_error_discard_the_answer_and_restore_editor() {
    for end in [
        Ok(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
        Err(std::io::Error::other("synthetic input failure")),
    ] {
        let failed = end.is_err();
        let mut shell = InteractiveShell::test_shell();
        shell.extension_set_editor("original draft".into());
        let request = ExtensionInputRequest {
            parent_request_id: 0,
            prompt: "API key (input hidden)".into(),
            secret: true,
        };
        let mut input =
            futures_util::stream::iter([Ok(Event::Paste("synthetic-private-key".into())), end]);
        let answer = extension_input_picker(&mut shell, &mut input, &request).await;
        if failed {
            assert!(answer.is_err());
        } else {
            assert_eq!(answer.unwrap(), None);
        }
        assert_eq!(shell.pending(), "original draft");
        let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
        assert!(!frame.contains("synthetic-private-key"));
        assert!(!frame.contains("API key (input hidden)"));
    }
}

#[tokio::test]
async fn oversized_secret_input_cannot_submit_a_truncated_key() {
    for oversized in [
        vec![Ok(Event::Paste("x".repeat(MAX_SECRET_INPUT_BYTES + 1)))],
        vec![
            Ok(Event::Paste("x".repeat(MAX_SECRET_INPUT_BYTES))),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            ))),
        ],
    ] {
        let mut shell = InteractiveShell::test_shell();
        let request = ExtensionInputRequest {
            parent_request_id: 0,
            prompt: "API key (input hidden)".into(),
            secret: true,
        };
        let mut events = oversized;
        events.extend([
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Ok(Event::Paste("replacement-key".into())),
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
        ]);
        let answer = extension_input_picker(
            &mut shell,
            &mut futures_util::stream::iter(events),
            &request,
        )
        .await
        .unwrap();
        assert_eq!(answer.as_deref(), Some("replacement-key"));
        assert!(shell.pending_is_empty());
    }
}

#[test]
fn secret_input_is_utf8_bounded_and_backspace_removes_one_character() {
    let mut value = SecretInputBuffer::default();
    value.extend_paste(&"x".repeat(MAX_SECRET_INPUT_BYTES - 1));
    value.push('é');
    assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES - 1);
    value.push('!');
    assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES);
    value.backspace();
    value.backspace();
    value.extend_paste("é\r\n");
    assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES);
    assert!(std::str::from_utf8(&value.0).unwrap().ends_with('é'));
    value.backspace();
    assert_eq!(value.0.len(), MAX_SECRET_INPUT_BYTES - 2);
}

#[test]
fn active_choice_is_focused_and_marked_without_reordering() {
    let mut labels = vec!["off".into(), "high".into(), "max".into()];
    assert_eq!(mark_current_choice(&mut labels, Some(2)), 2);
    assert_eq!(labels, ["off", "high", "max (current)"]);
    let mut empty = Vec::new();
    assert_eq!(mark_current_choice(&mut empty, None), 0);
}

#[tokio::test]
async fn preview_follows_navigation_and_filtered_original_indices_before_next_input() {
    use crate::tui::theme::{test_theme_for, TerminalBackground};
    use std::cell::RefCell;
    use std::task::Poll;

    let mut shell = InteractiveShell::test_shell();
    let original = shell.theme();
    let backgrounds = [
        TerminalBackground::Unknown,
        TerminalBackground::Light,
        TerminalBackground::Dark,
    ];
    let themes = backgrounds.map(|background| test_theme_for(background, original.capabilities()));
    let observed = RefCell::new(Vec::new());
    // Each expectation is checked when the stream is polled for the NEXT
    // event: the previous navigation must have already changed the theme.
    let mut script = [
        (Some(0), KeyCode::Down),
        (Some(1), KeyCode::Down),
        (Some(2), KeyCode::Up),
        (Some(1), KeyCode::Home),
        (Some(0), KeyCode::End),
        (Some(2), KeyCode::Char('t')),
        (Some(0), KeyCode::Char('e')),
        (Some(1), KeyCode::Down), // "te" matches Light and Dark terminal.
        (Some(2), KeyCode::Char('x')),
        (None, KeyCode::Enter), // Empty results cannot be confirmed.
        (None, KeyCode::Backspace),
        (Some(1), KeyCode::Down),
        (Some(2), KeyCode::Enter),
    ]
    .into_iter();
    let mut input = futures_util::stream::poll_fn(|_| {
        let Some((expected, code)) = script.next() else {
            return Poll::Ready(None);
        };
        let background = expected.map_or(original.background(), |index| backgrounds[index]);
        assert_eq!(observed.borrow().last(), Some(&(expected, background)));
        Poll::Ready(Some(Ok(Event::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        )))))
    });
    let items = vec![
        "Auto (recommended)".into(),
        "Light terminal".into(),
        "Dark terminal".into(),
    ];
    let action = PanelAction::ProviderSetup(items.clone());
    let selected = pick_list_with_preview(
        &mut shell,
        &mut input,
        OrdinarySurfaceMetadata::new("Terminal appearance"),
        items,
        vec![
            Some("neutral".into()),
            Some("daytime".into()),
            Some("nighttime".into()),
        ],
        0,
        action,
        |shell, index| {
            assert_eq!(shell.highlighted_panel_index(), index);
            shell.set_theme(index.map_or(&original, |index| &themes[index]).clone());
            observed
                .borrow_mut()
                .push((index, shell.theme().background()));
        },
    )
    .await
    .unwrap();

    assert_eq!(selected, Some(2));
    assert_eq!(shell.theme().background(), TerminalBackground::Dark);
    assert!(!shell.has_panel());
    assert_eq!(observed.borrow().len(), 12);
}

#[tokio::test]
async fn ordinary_provider_picker_navigation_does_not_change_theme() {
    let mut shell = InteractiveShell::test_shell();
    let original = shell.theme();
    let mut input = tokio_stream::iter([
        Ok(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))),
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
    ]);
    let selected = provider_setup_picker(
        &mut shell,
        &mut input,
        "Provider setup",
        vec!["one".into(), "two".into()],
        vec![None, None],
        0,
    )
    .await
    .unwrap();
    assert_eq!(selected, Some(1));
    assert_eq!(shell.theme().background(), original.background());
    assert_eq!(shell.theme().capabilities(), original.capabilities());
}

#[tokio::test]
async fn model_choice_starts_at_first_result_on_every_open_and_keeps_current_marker() {
    let catalog = ModelCatalog::builtin().unwrap();
    let presentation = model_picker_presentation(&catalog);
    let current = presentation.ids.last().unwrap().clone();
    assert_ne!(current, presentation.ids[0]);
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("test", &current.0, "high");

    // Exercise the real model driver, without the user-config persistence
    // boundary. Provider headings never occupy a selectable index.
    for size in [(46, 8), (80, 24), (120, 40)] {
        shell.set_size(size.0, size.1);
        for (keys, expected) in [
            (vec![KeyCode::Enter], Some(presentation.ids[0].clone())),
            (
                vec![KeyCode::Down, KeyCode::Enter],
                Some(presentation.ids[1].clone()),
            ),
            (
                "(current)"
                    .chars()
                    .map(KeyCode::Char)
                    .chain([KeyCode::Enter])
                    .collect(),
                Some(current.clone()),
            ),
            (vec![KeyCode::Esc], None),
            // Reopening clears the previous filter and navigation state.
            (vec![KeyCode::Enter], Some(presentation.ids[0].clone())),
        ] {
            let mut input = tokio_stream::iter(
                keys.into_iter()
                    .map(|key| Ok(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))),
            );
            assert_eq!(
                pick_model_choice(&mut shell, &mut input, &catalog)
                    .await
                    .unwrap(),
                expected,
                "model selection at {size:?}"
            );
            assert!(!shell.has_panel());
            assert_eq!(
                shell.selected_identity(),
                (current.0.clone(), "high".into())
            );
            assert!(shell.debug_snapshot().is_empty());
            assert_eq!(shell.debug_error(), None);
        }
    }
}

#[tokio::test]
async fn empty_model_choice_retains_the_availability_error() {
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending();
    assert_eq!(
        pick_model_choice(&mut shell, &mut input, &ModelCatalog::default())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        shell.debug_error().as_deref(),
        Some("nothing is available to select")
    );
    assert!(!shell.has_panel());
}

#[tokio::test]
async fn live_styled_document_rerenders_at_panel_content_width_after_resize() {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    sender.send(Ok(Event::Resize(44, 16))).await.unwrap();
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    drop(sender);
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 20);
    let widths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = std::sync::Arc::clone(&widths);

    read_only_document_live_styled(
        &mut shell,
        &mut input,
        "worker transcript",
        "initial".into(),
        move |width| {
            observed.lock().unwrap().push(width);
            std::future::ready(Ok(Some(format!("rendered at {width}"))))
        },
    )
    .await
    .unwrap();

    assert!(widths.lock().unwrap().contains(&44));
    assert!(!shell.has_panel());
}

#[tokio::test]
async fn ctrl_d_closes_a_picker_and_propagates_the_close_request() {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ))))
        .await
        .unwrap();
    drop(sender);
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();

    let selected = pick_list(
        &mut shell,
        &mut input,
        OrdinarySurfaceMetadata::new("Choose"),
        vec!["one".into()],
        vec![None],
        0,
        PanelAction::SelectModel(vec![ModelId("one".into())]),
    )
    .await
    .unwrap();

    assert_eq!(selected, None);
    assert!(!shell.has_panel());
    assert!(shell.close_requested());
}

#[tokio::test]
async fn message_picker_driver_returns_the_selected_message() {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    drop(sender);
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();
    let selected = message_picker(
        &mut shell,
        &mut input,
        vec![
            ForkMessage {
                entry_id: "entry-a".into(),
                text: "first".into(),
                whole_conversation: false,
            },
            ForkMessage {
                entry_id: "entry-b".into(),
                text: "second".into(),
                whole_conversation: false,
            },
        ],
    )
    .await
    .unwrap();

    assert_eq!(selected, Some(("entry-a".into(), "first".into())));
    assert!(!shell.has_panel());
}

struct LivePickerRefresh {
    calls: usize,
    refreshed: Option<tokio::sync::oneshot::Sender<()>>,
}

fn refresh_live_picker(
    context: &mut LivePickerRefresh,
) -> Pin<Box<dyn Future<Output = SubagentPickerSnapshot> + '_>> {
    Box::pin(async move {
        context.calls += 1;
        if let Some(refreshed) = context.refreshed.take() {
            let _ = refreshed.send(());
        }
        SubagentPickerSnapshot {
            title: "Subagents · refreshed".into(),
            items: vec!["beta".into(), "gamma".into()],
            descriptions: vec![Some("done".into()), Some("running".into())],
            node_ids: vec!["node-b".into(), "node-c".into()],
            groups: vec![
                SubagentGroup {
                    label: "Running".into(),
                    indices: vec![1],
                    collapsible: false,
                },
                SubagentGroup {
                    label: "Done".into(),
                    indices: vec![0],
                    collapsible: true,
                },
            ],
            notices: Vec::new(),
        }
    })
}

#[tokio::test]
async fn live_subagent_picker_refreshes_and_keeps_the_stable_selection() {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let (refreshed_tx, refreshed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        refreshed_rx
            .await
            .expect("picker refreshed before confirmation");
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
    });
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();
    let mut refresh = LivePickerRefresh {
        calls: 0,
        refreshed: Some(refreshed_tx),
    };
    let selected = subagent_picker(
        &mut shell,
        &mut input,
        SubagentPickerSnapshot {
            title: "Subagents".into(),
            items: vec!["alpha".into(), "beta".into()],
            descriptions: vec![Some("running".into()), Some("running".into())],
            node_ids: vec!["node-a".into(), "node-b".into()],
            groups: vec![SubagentGroup {
                label: "Running".into(),
                indices: vec![0, 1],
                collapsible: false,
            }],
            notices: Vec::new(),
        },
        1,
        &mut refresh,
        refresh_live_picker,
    )
    .await
    .unwrap();

    assert_eq!(selected.as_deref(), Some("node-b"));
    assert!(refresh.calls >= 1);
}

#[test]
fn model_label_uses_friendly_metadata_without_wire_id_noise() {
    let spec = octet_ai::ModelSpec {
        preset: Default::default(),
        id: ModelId("my-custom".into()),
        endpoint: octet_ai::EndpointId("local".into()),
        api_name: "llama-3.1-8b-instruct".into(),
        display_name: Some("Llama 3.1 8B".into()),
        protocol: octet_ai::Protocol::OpenAiChat,
        capabilities: octet_ai::Capabilities {
            responses_features: Default::default(),
            input_modalities: octet_ai::ModalitySet::none(),
            output_modalities: octet_ai::ModalitySet::none(),
            tools: true,
            parallel_tool_calls: false,
            reasoning: None,
            responses_lite: false,
            agent_delegation: None,
            structured_output: false,
            deferred_tool_loading: false,
        },
        limits: octet_ai::ModelLimits {
            context_window: 131072,
            max_output_tokens: 8192,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    };
    assert_eq!(model_label(&spec), "Llama 3.1 8B");
    assert_eq!(model_picker_metadata(&spec).input_cost, "—");

    let mut priced = spec.clone();
    priced.pricing = Some(octet_ai::Pricing {
        input: octet_ai::TokenRate(1_000_000),
        output: octet_ai::TokenRate(6_000_000),
        cache_read: octet_ai::TokenRate(100_000),
        cache_write_5m: octet_ai::TokenRate(1_250_000),
        cache_write_1h: None,
        reasoning: None,
        tiers: Vec::new(),
    });
    assert_eq!(compact_rate_value(octet_ai::TokenRate(0)), "$0");
    assert_eq!(compact_rate_value(octet_ai::TokenRate(100_000_000)), "$100");
    assert_eq!(compact_context_limit(1_500_000), "1.5M");
    let metadata = model_picker_metadata(&priced);
    assert_eq!(metadata.input_cost, "$1/M");
    assert_eq!(metadata.output_cost, "$6/M");
    assert_eq!(metadata.context, "131K");
    assert_eq!(metadata.media, "");
}

#[test]
fn custom_model_label_removes_provider_repository_and_quantization_noise() {
    let mut spec = octet_ai::ModelSpec {
        preset: Default::default(),
        id: ModelId("custom/Intel/Qwen3.6-27B-int4-AutoRound".into()),
        endpoint: octet_ai::EndpointId("custom-openai".into()),
        api_name: "Intel/Qwen3.6-27B-int4-AutoRound".into(),
        display_name: None,
        protocol: octet_ai::Protocol::OpenAiChat,
        capabilities: octet_ai::Capabilities {
            responses_features: Default::default(),
            input_modalities: octet_ai::ModalitySet::none(),
            output_modalities: octet_ai::ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
            reasoning: None,
            responses_lite: false,
            agent_delegation: None,
            structured_output: true,
            deferred_tool_loading: false,
        },
        limits: octet_ai::ModelLimits {
            context_window: 128000,
            max_output_tokens: 16384,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    };
    assert_eq!(model_label(&spec), "Qwen3.6 27B");

    spec.capabilities.input_modalities = octet_ai::ModalitySet::none()
        .with(octet_ai::Modality::Image)
        .with(octet_ai::Modality::Audio);

    let metadata = model_picker_metadata(&spec);
    assert_eq!(metadata.media, "vision + audio");
}

#[test]
fn model_picker_groups_and_sorts_models_with_stable_metadata_columns() {
    let catalog = ModelCatalog::builtin().unwrap();
    let presentation = model_picker_presentation(&catalog);
    let groups = presentation
        .providers
        .iter()
        .fold(Vec::<&str>::new(), |mut groups, provider| {
            if groups.last().copied() != Some(provider.as_str()) {
                groups.push(provider);
            }
            groups
        });
    assert_eq!(groups, vec!["Anthropic", "OpenAI"]);

    for provider in &groups {
        let labels = presentation
            .labels
            .iter()
            .zip(&presentation.providers)
            .filter(|(_, row_provider)| row_provider.as_str() == *provider)
            .map(|(label, _)| label.to_lowercase())
            .collect::<Vec<_>>();
        assert!(
            labels.windows(2).all(|pair| pair[0] <= pair[1]),
            "{provider} models were not alphabetized: {labels:?}"
        );
    }

    let descriptions = presentation
        .descriptions
        .iter()
        .map(|description| description.as_deref().unwrap())
        .collect::<Vec<_>>();
    // Unpriced rows omit `in`/`out` entirely; column stability only
    // applies to the priced rows that still show both costs.
    let priced: Vec<&&str> = descriptions
        .iter()
        .filter(|description| description.contains("out "))
        .collect();
    assert!(!priced.is_empty(), "expected priced builtin models");
    let out_columns = priced
        .iter()
        .map(|description| {
            sexy_tui_rs::visible_width(&description[..description.find("out ").unwrap()])
        })
        .collect::<std::collections::BTreeSet<_>>();
    let context_columns = descriptions
        .iter()
        .map(|description| {
            sexy_tui_rs::visible_width(&description[..description.find(" ctx").unwrap()])
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(out_columns.len(), 1, "input costs are not one fixed column");
    assert_eq!(
        context_columns.len(),
        1,
        "context windows are not one fixed column"
    );
    assert!(descriptions
        .iter()
        .any(|description| description.contains("audio")));
    assert!(descriptions
        .iter()
        .any(|description| description.contains("vision")));

    let astra_index = presentation
        .ids
        .iter()
        .position(|id| id.0 == "gpt-6-astra")
        .expect("built-in Astra should use the generic model picker path");
    assert_eq!(presentation.providers[astra_index], "OpenAI");
    assert_eq!(presentation.labels[astra_index], "GPT-6 Astra");
    let astra_description = descriptions[astra_index];
    assert!(astra_description.contains("$10/M"));
    assert!(astra_description.contains("$50/M"));
    assert!(astra_description.contains("1.1M ctx"));
    assert!(astra_description.contains("vision"));

    assert!(descriptions.iter().all(|description| {
        !description.contains("tools")
            && !description.contains("reasoning")
            && !description.contains("Anthropic")
            && !description.contains("OpenAI")
    }));
}

#[test]
fn model_picker_omits_unknown_pricing_dashes() {
    let source = ModelCatalog::builtin().unwrap();
    let template = source.models().next().unwrap().clone();
    let mut catalog = ModelCatalog::default();
    let endpoint = source.resolve(&template.id).unwrap().endpoint.clone();
    catalog.register_endpoint((*endpoint).clone()).unwrap();
    let mut subscription = template.clone();
    subscription.id = ModelId("subscription-model".into());
    subscription.display_name = Some("Subscription Model".into());
    subscription.pricing = None;
    catalog.register_model(subscription).unwrap();

    let presentation = model_picker_presentation(&catalog);
    assert_eq!(presentation.ids.len(), 1);
    let description = presentation.descriptions[0].as_deref().unwrap();
    assert!(
        !description.contains("in "),
        "unknown pricing should omit `in`: {description:?}"
    );
    assert!(
        !description.contains("out "),
        "unknown pricing should omit `out`: {description:?}"
    );
    assert!(!description.contains('—'), "{description:?}");
    assert!(description.contains("ctx"), "{description:?}");
}

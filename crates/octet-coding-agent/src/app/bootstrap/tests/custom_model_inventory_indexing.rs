//! Indexed custom-model merge correctness and work budgets.

use super::*;

#[test]
fn indexed_custom_merge_preserves_duplicates_case_identity_and_all_metadata() {
    use crate::auth::custom::{CustomModel, CustomPricing};

    let first = CustomModel {
        api_name: "model".into(),
        display_name: "First configured model".into(),
        context_window: 32_000,
        max_output_tokens: 8_192,
        tools: true,
        parallel_tool_calls: true,
        vision: true,
        structured_output: true,
        reasoning: true,
        reasoning_profile: Some(OpenAiChatReasoningMode::QwenEnableThinking),
        reasoning_source: Some(octet_ai::types::ReasoningMetadataSource::Explicit),
        reasoning_values: vec!["none".into(), "default".into()],
        reasoning_default: "default".into(),
        reasoning_uses_system_message: true,
        pricing: Some(CustomPricing {
            input: 75,
            output: 300,
            cache_read: 8,
            cache_write_5m: 19,
        }),
        preset: octet_ai::ModelPreset {
            vllm_priority: Some(2),
            supports_max_output_tokens: Some(false),
            sampling_params: [("temperature".into(), serde_json::json!(0.25))].into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let manual = CustomModel {
        api_name: "manual".into(),
        display_name: "First manual model".into(),
        ..Default::default()
    };
    let case_distinct = CustomModel {
        api_name: "Model".into(),
        display_name: "Case-distinct configured model".into(),
        context_window: 4_000,
        max_output_tokens: 1_000,
        tools: false,
        ..Default::default()
    };
    let manual_case = CustomModel {
        api_name: "MANUAL".into(),
        ..Default::default()
    };
    let configured = vec![
        first.clone(),
        manual.clone(),
        CustomModel {
            api_name: first.api_name.clone(),
            display_name: "Later configured duplicate must not win".into(),
            ..Default::default()
        },
        case_distinct.clone(),
        CustomModel {
            display_name: "Later manual duplicate must not be appended".into(),
            ..manual.clone()
        },
        manual_case.clone(),
    ];
    let unconfigured = CustomModel {
        api_name: "unconfigured".into(),
        display_name: "First inventory entry".into(),
        context_window: 12_000,
        max_output_tokens: 3_000,
        reasoning: true,
        ..Default::default()
    };
    let unconfigured_duplicate = CustomModel {
        display_name: "Second inventory entry must survive".into(),
        context_window: 24_000,
        ..unconfigured.clone()
    };
    let uppercase = CustomModel {
        api_name: "MODEL".into(),
        display_name: "Unconfigured case-distinct model".into(),
        ..Default::default()
    };
    let discovered = vec![
        unconfigured.clone(),
        CustomModel {
            api_name: first.api_name.clone(),
            context_window: 4_096,
            max_output_tokens: 4_000,
            context_window_asserted: true,
            max_output_tokens_asserted: true,
            ..Default::default()
        },
        CustomModel {
            api_name: first.api_name.clone(),
            context_window: 999_999,
            max_output_tokens: 1,
            ..Default::default()
        },
        CustomModel {
            api_name: case_distinct.api_name.clone(),
            context_window: 512,
            max_output_tokens: 9_999,
            context_window_asserted: true,
            ..Default::default()
        },
        unconfigured_duplicate.clone(),
        uppercase.clone(),
    ];
    for auto_discover in [false, true] {
        let mut expected = vec![
            unconfigured.clone(),
            first.clone(),
            first.clone(),
            case_distinct.clone(),
            unconfigured_duplicate.clone(),
            uppercase.clone(),
            manual.clone(),
            manual_case.clone(),
        ];
        if auto_discover {
            expected[1].context_window = 4_096;
            expected[1].max_output_tokens = 4_000;
            expected[1].context_window_asserted = true;
            expected[1].max_output_tokens_asserted = true;
            expected[3].context_window = 512;
            expected[3].max_output_tokens = 512;
            expected[3].context_window_asserted = true;
        }
        let merged =
            apply_configured_custom_model_overrides(discovered.clone(), &configured, auto_discover);
        // Compare every field, not just names and limits: indexed lookup must
        // not alter pricing, reasoning, capabilities, presets, or provenance.
        assert_eq!(
            serde_json::to_value(&merged).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "auto_discover={auto_discover}"
        );
        let unchanged =
            apply_configured_custom_model_overrides(discovered.clone(), &[], auto_discover);
        assert_eq!(
            serde_json::to_value(unchanged).unwrap(),
            serde_json::to_value(&discovered).unwrap()
        );
    }
}

#[test]
fn many_custom_models_use_a_linear_index_work_budget() {
    use crate::auth::custom::CustomModel;
    use std::cell::Cell;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{BuildHasher, Hasher};
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct HashWork(Rc<Cell<usize>>);
    struct CountedHasher(DefaultHasher, Rc<Cell<usize>>);
    impl BuildHasher for HashWork {
        type Hasher = CountedHasher;
        fn build_hasher(&self) -> CountedHasher {
            CountedHasher(DefaultHasher::new(), self.0.clone())
        }
    }
    impl Hasher for CountedHasher {
        fn finish(&self) -> u64 {
            self.1.set(self.1.get() + 1);
            self.0.finish()
        }
        fn write(&mut self, bytes: &[u8]) {
            self.0.write(bytes);
        }
    }

    const COUNT: usize = 4_096;
    let mut configured = (0..COUNT)
        .map(|index| CustomModel {
            api_name: format!("model-{index:05}"),
            display_name: format!("Configured {index}"),
            context_window: 32_000,
            max_output_tokens: 8_192,
            ..Default::default()
        })
        .collect::<Vec<_>>();
    for index in (0..COUNT).step_by(128) {
        configured.push(CustomModel {
            display_name: "Later configured duplicate".into(),
            ..configured[index].clone()
        });
    }
    let partial = (0..COUNT / 2)
        .rev()
        .map(|index| CustomModel {
            api_name: configured[index].api_name.clone(),
            context_window: 1_024 + index as u64,
            max_output_tokens: 512,
            context_window_asserted: true,
            max_output_tokens_asserted: true,
            ..Default::default()
        })
        .collect::<Vec<_>>();
    // Empty discovery covers manual append; cloning configured inventory
    // mirrors offline registration. Partial inventory exercises both paths.
    for discovered in [Vec::new(), configured.clone(), partial] {
        for auto_discover in [false, true] {
            let work = HashWork::default();
            let input_len = configured.len() + discovered.len();
            let merged = merge_custom_models_with_overrides(
                discovered.clone(),
                &configured,
                auto_discover,
                work.clone(),
            );
            // Count real index hashes with a deterministic hasher, never wall
            // time. Preallocation leaves a bounded amount of work per entry.
            assert!(work.0.get() >= input_len);
            assert!(work.0.get() <= 4 * input_len, "{} hashes", work.0.get());
            let expected_len = if discovered.len() == configured.len() {
                configured.len()
            } else {
                COUNT
            };
            assert_eq!(merged.len(), expected_len);
            for (position, model) in merged.iter().enumerate() {
                let source_index = if discovered.is_empty() {
                    position
                } else if discovered.len() == configured.len() {
                    if position < COUNT {
                        position
                    } else {
                        (position - COUNT) * 128
                    }
                } else if position < COUNT / 2 {
                    COUNT / 2 - 1 - position
                } else {
                    position
                };
                let first = &configured[source_index];
                assert_eq!(model.api_name, first.api_name);
                assert_eq!(model.display_name, first.display_name);
                if auto_discover && discovered.len() == COUNT / 2 && position < COUNT / 2 {
                    assert_eq!(model.context_window, 1_024 + source_index as u64);
                    assert_eq!(model.max_output_tokens, 512);
                    assert!(model.context_window_asserted);
                    assert!(model.max_output_tokens_asserted);
                } else {
                    assert_eq!(model.context_window, first.context_window);
                    assert_eq!(model.max_output_tokens, first.max_output_tokens);
                    assert!(!model.context_window_asserted);
                    assert!(!model.max_output_tokens_asserted);
                }
            }
        }
    }
}

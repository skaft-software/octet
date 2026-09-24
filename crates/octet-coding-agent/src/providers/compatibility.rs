//! Prompt-cache compatibility selected by provider declarations.

use octet_ai::{CacheCompatibility, CacheControlFormat, Protocol, SessionAffinityFormat};

use super::contract::CompatibilityProfile;

/// Return the tested prompt-cache compatibility for a declaration-selected
/// provider route. The OpenCode header helper also checks the declared route
/// identity because this one routing header spans three different codecs.
pub(crate) fn cache_compatibility(
    profile: CompatibilityProfile,
    model_id: &str,
    protocol: Protocol,
) -> CacheCompatibility {
    let mut cache = CacheCompatibility::default();

    match profile {
        CompatibilityProfile::OpenAi => {
            cache.send_session_affinity_headers = true;
            cache.session_affinity_format = Some(SessionAffinityFormat::OpenAi);
            // Discovery and static registration share this exact public-API
            // contract. A copied name on another provider is not sufficient.
            cache.supports_explicit_prompt_cache_mode = protocol == Protocol::OpenAiResponses
                && matches!(model_id, "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna");
        }
        // OpenRouter forwards Anthropic's explicit cache-control blocks only
        // for its Anthropic routes. These markers are required for prompt
        // caching there; regular OpenAI-compatible routes use their defaults.
        CompatibilityProfile::OpenRouter => {
            cache.send_session_affinity_headers = true;
            cache.session_affinity_format = Some(SessionAffinityFormat::OpenRouter);
            if model_id.starts_with("anthropic/") {
                cache.cache_control_format = Some(CacheControlFormat::Anthropic);
            }
        }
        // These OpenAI-compatible providers reject the 24-hour retention
        // parameter. Short retention remains enabled.
        CompatibilityProfile::ShortRetention => {
            cache.supports_long_retention = false;
        }
        // Fireworks' Anthropic Messages routes require routing affinity and
        // accept cache controls on system/conversation blocks but reject them
        // on tool definitions.
        CompatibilityProfile::Fireworks if protocol == Protocol::AnthropicMessages => {
            cache.supports_long_retention = false;
            cache.send_session_affinity_headers = true;
            cache.supports_cache_control_on_tools = false;
        }
        CompatibilityProfile::OpenCode => {
            // Routing affinity applies to every OpenCode route independently of
            // the smaller set of models that reject long cache retention.
            cache.send_session_affinity_headers = true;
            if matches!(
                model_id,
                "deepseek-v4-flash"
                    | "deepseek-v4-pro"
                    | "kimi-k2.5"
                    | "kimi-k2.6"
                    | "minimax-m2.7"
            ) {
                cache.supports_long_retention = false;
            }
        }
        CompatibilityProfile::Codex => {
            cache.supports_long_retention = false;
            cache.send_session_id_header = false;
            cache.send_session_affinity_headers = true;
            cache.session_affinity_format = Some(SessionAffinityFormat::Codex);
        }
        CompatibilityProfile::Mistral => {
            cache.send_session_affinity_headers = true;
            cache.session_affinity_format = Some(SessionAffinityFormat::Mistral);
        }
        CompatibilityProfile::Google => {
            cache.supports_long_retention = false;
            cache.send_session_id_header = false;
            cache.supports_cache_control_on_tools = false;
        }
        CompatibilityProfile::Default
        | CompatibilityProfile::Cloudflare
        | CompatibilityProfile::Fireworks
        | CompatibilityProfile::Custom => {}
    }

    // OpenCode's known Responses routes use Pi's `openai-nosession` variant:
    // retain request affinity but omit the unsupported `session_id` header.
    if profile == CompatibilityProfile::OpenCode
        && protocol == Protocol::OpenAiResponses
        && (model_id.starts_with("gpt-") || model_id.starts_with("codex-"))
    {
        cache.send_session_id_header = false;
        cache.session_affinity_format = Some(SessionAffinityFormat::OpenAiNoSession);
    }

    cache
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::contract::{
        BASETEN, FIREWORKS, MISTRAL, OPENAI, OPENCODE, OPENCODE_GO, OPENROUTER,
    };

    #[test]
    fn explicit_cache_mode_requires_verified_public_openai_responses_model() {
        for id in ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna"] {
            assert!(
                cache_compatibility(OPENAI.compatibility, id, Protocol::OpenAiResponses)
                    .supports_explicit_prompt_cache_mode
            );
            assert!(
                !cache_compatibility(OPENCODE.compatibility, id, Protocol::OpenAiResponses)
                    .supports_explicit_prompt_cache_mode
            );
            assert!(
                !cache_compatibility(OPENROUTER.compatibility, id, Protocol::OpenAiResponses)
                    .supports_explicit_prompt_cache_mode
            );
            assert!(
                !cache_compatibility(OPENAI.compatibility, id, Protocol::OpenAiChat)
                    .supports_explicit_prompt_cache_mode
            );
        }
        for id in ["gpt-6-unverified", "openai/gpt-6-astra", "gpt-5.4"] {
            assert!(
                !cache_compatibility(OPENAI.compatibility, id, Protocol::OpenAiResponses)
                    .supports_explicit_prompt_cache_mode
            );
        }
    }

    #[test]
    fn generated_profiles_preserve_known_route_behavior() {
        let openai =
            cache_compatibility(OPENAI.compatibility, "gpt-5.4", Protocol::OpenAiResponses);
        assert_eq!(
            openai.session_affinity_format,
            Some(SessionAffinityFormat::OpenAi)
        );

        let openrouter = cache_compatibility(
            OPENROUTER.compatibility,
            "anthropic/claude-sonnet-4-5",
            Protocol::OpenAiChat,
        );
        assert_eq!(
            openrouter.cache_control_format,
            Some(CacheControlFormat::Anthropic)
        );

        let fireworks = cache_compatibility(
            FIREWORKS.compatibility,
            "accounts/fireworks/models/kimi-k2p7-code",
            Protocol::AnthropicMessages,
        );
        assert!(!fireworks.supports_cache_control_on_tools);

        let mistral = cache_compatibility(
            MISTRAL.compatibility,
            "mistral-small-latest",
            Protocol::OpenAiChat,
        );
        assert_eq!(
            mistral.session_affinity_format,
            Some(SessionAffinityFormat::Mistral)
        );

        let baseten = cache_compatibility(BASETEN.compatibility, "model", Protocol::OpenAiChat);
        assert!(baseten.send_session_affinity_headers);
        assert_eq!(
            baseten.session_affinity_format,
            Some(SessionAffinityFormat::OpenAi)
        );

        let opencode =
            cache_compatibility(OPENCODE.compatibility, "gpt-5.4", Protocol::OpenAiResponses);
        assert!(!opencode.send_session_id_header);
        assert!(opencode.send_session_affinity_headers);
        for protocol in [
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
            Protocol::GoogleGenerativeAi,
        ] {
            let affinity = cache_compatibility(OPENCODE.compatibility, "any-model", protocol);
            assert!(affinity.send_session_affinity_headers);
            assert!(affinity.supports_long_retention);
        }
        let go = cache_compatibility(OPENCODE_GO.compatibility, "kimi-k2.5", Protocol::OpenAiChat);
        assert!(go.send_session_affinity_headers);
        assert!(!go.supports_long_retention);
    }
}

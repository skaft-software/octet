#![allow(missing_docs)]

// Verify that all public API re-exports are accessible and compile.
#[allow(unused_imports)]
use octet_ai::{
    anthropic_bearer_auth, environment_variable_present, first_present_variable,
    reduce_assistant_message_frames, select_vertex_credential, vertex_api_key_auth, AiClient,
    AiError, AssistantMessage, AssistantMessageFrame, AssistantMessageFrameEncoder, AssistantPart,
    AudioFormat, AudioMedia, AudioOutputOptions, AudioPayload, AudioVoice, Auth, AuthConfig,
    AuthError, Capabilities, CatalogConfig, CompatibilityMode, ConfigError, Cost,
    CredentialResolver, CredentialResolverRegistry, CredentialScheme, DecodeError, DeferredHandle,
    DeferredHandleRejection, DeferredPollPermit, DeferredPollRefusalKind, Diagnostic, Endpoint,
    EndpointConfig, EndpointId, FauxDeferredStatus, FauxMessage, FauxOptions, FauxProvider,
    FauxResponse, FauxState, FauxToolCall, GeneratedImage, HeaderTransform, HookModelContext,
    HostRequestOptions, HttpError, ImageApi, ImageCancellation, ImageDetail,
    ImageGenerationOptions, ImageGenerationRequest, ImageGenerationResponse, ImageInput,
    ImageMedia, ImageModality, ImageModel, ImageModelCatalog, ImageModelSpec, ImageOutput,
    ImagePricing, ImageSource, ImageStopReason, JsonSchemaFormat, Media, Message, Mime, Modality,
    ModalitySet, Model, ModelCatalog, ModelConfig, ModelId, ModelLimits, ModelSpec, OutputFormat,
    OutputModalities, PayloadHook, Pricing, PricingError, PricingTier, Protocol, ProviderError,
    ProviderMediaRef, ReasoningCapability, ReasoningConfig, ReasoningControl, ReasoningEffort,
    ReasoningEffortBudgets, ReasoningPart, ReasoningState, ReasoningStateKind, Request,
    RequestBodyEncoding, RequestOverrides, RequestRuntime, ResolvedCredential, Response,
    ResponseHook, ResponseStream, ResponsesRuntimeProfile, Secret, StopReason, StreamEvent,
    StreamProtocolError, TokenRate, ToolCall, ToolCallId, ToolChoice, ToolDef, ToolResult,
    ToolResultPart, TransportError, TransportPhase, UnsupportedError, Usage, UserMessage, UserPart,
    ValidationError, VertexCredential,
};

// A compile-time proof that every public re-export above is nameable. Referencing
// `ReasoningStateKind` here also guards against it silently dropping out of the
// public surface (previously absent from this test).
const _: fn() = || {
    fn assert_exported<T>() {}
    assert_exported::<ReasoningStateKind>();
    assert_exported::<ReasoningState>();
    assert_exported::<DeferredHandle>();
    assert_exported::<DeferredPollPermit>();
    assert_exported::<VertexCredential>();
    assert_exported::<HookModelContext>();
    assert_exported::<HostRequestOptions>();
    assert_exported::<ImageModelCatalog>();
    assert_exported::<ImageGenerationResponse>();
    assert_exported::<ImageCancellation>();
    assert_exported::<FauxProvider>();
};

#[test]
fn test_public_api_secret_redaction_proof() {
    let secret = Secret::from("my-super-secret-key-12345");

    // Debug and Display must redact
    let debug_str = format!("{:?}", secret);
    let display_str = format!("{}", secret);

    assert!(!debug_str.contains("my-super-secret-key-12345"));
    assert!(!display_str.contains("my-super-secret-key-12345"));

    assert!(debug_str.contains("<redacted>"));
    assert!(display_str.contains("<redacted>"));

    // Auth enum containing secrets must redact in Debug
    let auth = Auth::bearer("my-super-secret-key-12345");
    let auth_debug = format!("{:?}", auth);
    assert!(!auth_debug.contains("my-super-secret-key-12345"));
    assert!(auth_debug.contains("<redacted>"));

    // Endpoint containing secrets must redact in Debug
    let ep = Endpoint {
        id: EndpointId("ep-1".to_string()),
        base_url: url::Url::parse("https://api.openai.com/").unwrap(),
        auth,
        default_headers: http::HeaderMap::new(),
        transport: octet_ai::EndpointTransport::Http,
        runtime: octet_ai::RequestRuntime::default(),
        timeout: std::time::Duration::from_secs(30),
    };
    let ep_debug = format!("{:?}", ep);
    assert!(!ep_debug.contains("my-super-secret-key-12345"));
}

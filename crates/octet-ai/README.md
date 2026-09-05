# octet-ai

Provider-independent inference for octet's agent loop.

`octet-ai` provides one canonical conversation model and one event stream across:

- OpenAI Chat Completions
- OpenAI Responses
- Anthropic Messages
- Amazon Bedrock Converse
- Google generateContent

The crate supports tools, reasoning continuation state, images, Chat conversational audio, structured output, strict/lossy cross-protocol conversion, dynamic authentication, integer usage pricing, custom endpoints, cancellation by stream drop, and an embedded offline model catalog.

Feature support varies by protocol; this list does not imply uniform capabilities
or live-provider acceptance across all five.

See the [AI design](../../docs/design/octet-ai.md) and the crate-level Rust documentation for the public API.

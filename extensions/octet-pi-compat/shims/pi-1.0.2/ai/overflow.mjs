
const OVERFLOW_PATTERNS = [
    /prompt (?:is )?too long/i, // Anthropic and z.ai token overflow
    /prompt exceeds max length/i, // z.ai CN endpoint token overflow
    /request_too_large/i, // Anthropic request byte-size overflow (HTTP 413)
    /input is too long for requested model/i, // Amazon Bedrock
    /exceeds the context window/i, // OpenAI (Completions & Responses API)
    /exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))/i, // OpenAI-compatible proxies (LiteLLM)
    /input token count.*exceeds the maximum/i, // Google (Gemini)
    /maximum prompt length is \d+/i, // xAI (Grok)
    /reduce the length of the messages/i, // Groq
    /maximum context length is \d+ tokens/i, // OpenRouter (most backends)
    /exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?/i, // OpenRouter/Poolside
    /input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)/i, // Together AI
    /exceeds the limit of \d+/i, // GitHub Copilot
    /exceeds the available context size/i, // llama.cpp server
    /greater than the context length/i, // LM Studio
    /context window exceeds limit/i, // MiniMax
    /exceeded model token limit/i, // Kimi For Coding
    /too large for model with \d+ maximum context length/i, // Mistral
    /prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?/i, // DS4 server
    /model_context_window_exceeded/i, // z.ai non-standard finish_reason surfaced as error text
    /prompt too long; exceeded (?:max )?context length/i, // Ollama explicit overflow error
    /range of input length should be/i, // DashScope / Qwen Token Plan
    /context[_ ]length[_ ]exceeded/i, // Generic fallback
    /too many tokens/i, // Generic fallback
    /token limit exceeded/i, // Generic fallback
];
const CEREBRAS_BODYLESS_OVERFLOW_PATTERN = /^4(?:00|13)\s*(?:status code)?\s*\(no body\)/i;

const NON_OVERFLOW_PATTERNS = [
    /^(Throttling error|Service unavailable):/i, // AWS Bedrock non-overflow errors (human-readable prefixes from formatBedrockError)
    /rate limit/i, // Generic rate limiting
    /too many requests/i, // Generic HTTP 429 style
];

export function isContextOverflow(message, contextWindow) {
    if (message.stopReason === "error" && message.errorMessage) {
        const isNonOverflow = NON_OVERFLOW_PATTERNS.some((p) => p.test(message.errorMessage));
        if (!isNonOverflow) {
            if (OVERFLOW_PATTERNS.some((p) => p.test(message.errorMessage))) {
                return true;
            }
            if (message.provider === "cerebras" && CEREBRAS_BODYLESS_OVERFLOW_PATTERN.test(message.errorMessage)) {
                return true;
            }
        }
    }
    if (contextWindow && message.stopReason === "stop") {
        const inputTokens = message.usage.input + message.usage.cacheRead;
        if (inputTokens > contextWindow) {
            return true;
        }
    }
    if (contextWindow && message.stopReason === "length" && message.usage.output === 0) {
        const inputTokens = message.usage.input + message.usage.cacheRead;
        if (inputTokens >= contextWindow * 0.99) {
            return true;
        }
    }
    return false;
}

export function isRecoverableLength(message, desiredMaxOutput) {
    return message.stopReason === "length" && desiredMaxOutput > 0 && message.usage.output < desiredMaxOutput;
}

export function getOverflowPatterns() {
    return [...OVERFLOW_PATTERNS];
}

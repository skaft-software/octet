
const OVERFLOW_PATTERNS = [
    /prompt (?:is )?too long/i,
    /prompt exceeds max length/i,
    /request_too_large/i,
    /input is too long for requested model/i,
    /exceeds the context window/i,
    /exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))/i,
    /input token count.*exceeds the maximum/i,
    /maximum prompt length is \d+/i,
    /reduce the length of the messages/i,
    /maximum context length is \d+ tokens/i,
    /exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?/i,
    /input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)/i,
    /exceeds the limit of \d+/i,
    /exceeds the available context size/i,
    /greater than the context length/i,
    /context window exceeds limit/i,
    /exceeded model token limit/i,
    /too large for model with \d+ maximum context length/i,
    /prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?/i,
    /model_context_window_exceeded/i,
    /prompt too long; exceeded (?:max )?context length/i,
    /range of input length should be/i,
    /context[_ ]length[_ ]exceeded/i,
    /too many tokens/i,
    /token limit exceeded/i,
];
const CEREBRAS_BODYLESS_OVERFLOW_PATTERN = /^4(?:00|13)\s*(?:status code)?\s*\(no body\)/i;

const NON_OVERFLOW_PATTERNS = [
    /^(Throttling error|Service unavailable):/i,
    /rate limit/i,
    /too many requests/i,
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

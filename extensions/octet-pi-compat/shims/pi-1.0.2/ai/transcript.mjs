import { contentText, getSystemMessageText } from "./text.mjs";

export function createInitialSystemMessage(systemPrompt, tools) {
    const hasSystemPrompt = systemPrompt !== undefined && systemPrompt.length > 0;
    const hasTools = tools !== undefined && tools.length > 0;
    if (!hasSystemPrompt && !hasTools)
        return undefined;
    return {
        role: "system",
        content: systemPrompt ?? "",
        ...(hasTools ? { toolsAdded: tools } : {}),
        timestamp: 0,
    };
}

export function normalizeContext(context) {
    const initialMessage = createInitialSystemMessage(context.systemPrompt, context.tools);
    const messages = initialMessage ? [initialMessage, ...context.messages] : context.messages;
    return { messages };
}
function isSystemMessage(message) {
    return message.role === "system";
}

export function getInitialSystemMessage(messages) {
    const first = messages[0];
    return first && isSystemMessage(first) ? first : undefined;
}

export function withoutInitialSystemMessage(messages) {
    return getInitialSystemMessage(messages) ? messages.slice(1) : messages;
}

export function getCurrentTools(messages) {
    const tools = new Map();
    for (const message of messages) {
        if (!isSystemMessage(message))
            continue;
        for (const tool of message.toolsRemoved ?? [])
            tools.delete(tool.name);
        for (const tool of message.toolsAdded ?? [])
            tools.set(tool.name, tool);
    }
    return [...tools.values()];
}

export function getCurrentSystemMessage(messages) {
    const content = [];
    const sections = new Map();
    let timestamp;
    for (const message of messages) {
        if (!isSystemMessage(message))
            continue;
        timestamp ??= message.timestamp;
        const text = contentText(message.content);
        if (text.length > 0)
            content.push(text);
        for (const [name, value] of Object.entries(message.sections ?? {})) {
            if (value === null)
                sections.delete(name);
            else
                sections.set(name, value);
        }
    }
    const tools = getCurrentTools(messages);
    if (timestamp === undefined && tools.length === 0)
        return undefined;
    return {
        role: "system",
        content: content.join("\n\n"),
        ...(sections.size > 0 ? { sections: Object.fromEntries(sections) } : {}),
        ...(tools.length > 0 ? { toolsAdded: tools } : {}),
        timestamp: timestamp ?? 0,
    };
}

export function getCurrentSystemPrompt(messages) {
    const message = getCurrentSystemMessage(messages);
    return message ? getSystemMessageText(message) : "";
}

export function collapseSystemMessages(context) {
    const head = getCurrentSystemMessage(context.messages);
    const messages = context.messages.filter((message) => message.role !== "system");
    return { messages: head ? [head, ...messages] : messages };
}

export function resolveTranscript(context, supportsMidConvoSystemMessages) {
    return supportsMidConvoSystemMessages ? context : collapseSystemMessages(context);
}

export function toToolDeclaration(tool) {
    return {
        name: tool.name,
        description: tool.description,
        parameters: JSON.parse(JSON.stringify(tool.parameters)),
        ...(tool.constrainedSampling === undefined ? {} : { constrainedSampling: tool.constrainedSampling }),
    };
}

export function declarationsEqual(left, right) {
    return JSON.stringify(toToolDeclaration(left)) === JSON.stringify(toToolDeclaration(right));
}

export function getToolStateChanges(previous, current) {
    const previousTools = new Map(previous.map((tool) => [tool.name, tool]));
    const currentTools = new Map(current.map((tool) => [tool.name, tool]));
    return {
        toolsAdded: current
            .filter((tool) => {
            const previousTool = previousTools.get(tool.name);
            return previousTool === undefined || !declarationsEqual(previousTool, tool);
        })
            .map(toToolDeclaration),
        toolsRemoved: previous
            .filter((tool) => {
            const currentTool = currentTools.get(tool.name);
            return currentTool === undefined || !declarationsEqual(tool, currentTool);
        })
            .map((tool) => ({ name: tool.name })),
    };
}

export function getDeclaredTools(messages) {
    const definitions = new Map();
    for (const message of messages) {
        if (!isSystemMessage(message))
            continue;
        for (const tool of message.toolsAdded ?? [])
            definitions.set(tool.name, tool);
    }
    return [...definitions.values()];
}

export function hasToolRedefinitions(messages) {
    const declared = new Map();
    for (const message of messages) {
        if (!isSystemMessage(message))
            continue;
        for (const tool of message.toolsAdded ?? []) {
            const previous = declared.get(tool.name);
            if (previous !== undefined && !declarationsEqual(previous, tool))
                return true;
            declared.set(tool.name, tool);
        }
    }
    return false;
}

export function hasNonAdditiveToolChanges(messages) {
    const declared = new Set();
    for (const message of messages) {
        if (!isSystemMessage(message))
            continue;
        if ((message.toolsRemoved?.length ?? 0) > 0)
            return true;
        for (const tool of message.toolsAdded ?? []) {
            if (declared.has(tool.name))
                return true;
            declared.add(tool.name);
        }
    }
    return false;
}

export function resolveTranscriptTools(messages, supportsToolAdditions) {
    const anchorsAdditions = supportsToolAdditions && !hasNonAdditiveToolChanges(messages);
    return {
        requestTools: anchorsAdditions
            ? (getInitialSystemMessage(messages)?.toolsAdded ?? [])
            : getCurrentTools(messages),
        anchorsAdditions,
    };
}

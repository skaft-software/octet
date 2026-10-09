// Pure helpers adapted from Pi 1.0.2, commit cd32f7725fdbddbaecdff5b1e68491563394e0ca:
// packages/coding-agent/src/core/{compaction/compaction,session-manager,messages}.ts
// Copyright (c) 2025 Mario Zechner. MIT; see ../LICENSE.pi.
// These operate on caller-supplied Pi data, not octet's live/canonical session.

export function calculateContextTokens(usage) {
  return usage.totalTokens > 0
    ? usage.totalTokens
    : usage.input + usage.output + usage.cacheRead + usage.cacheWrite;
}

function estimateTextAndImageContentChars(content) {
  if (typeof content === 'string') return content.length;
  let chars = 0;
  for (const block of content) {
    if (block.type === 'text' && block.text) chars += block.text.length;
    else if (block.type === 'image') chars += 4800;
  }
  return chars;
}

export function estimateTokens(message) {
  let chars = 0;
  switch (message.role) {
    case 'system':
      chars = estimateTextAndImageContentChars(message.content);
      for (const section of Object.values(message.sections ?? {})) if (section) chars += section.length;
      if (message.toolsAdded) chars += JSON.stringify(message.toolsAdded).length;
      break;
    case 'user':
    case 'custom':
    case 'toolResult':
      chars = estimateTextAndImageContentChars(message.content);
      break;
    case 'assistant':
      for (const block of message.content) {
        if (block.type === 'text') chars += block.text.length;
        else if (block.type === 'thinking') chars += block.thinking.length;
        else if (block.type === 'toolCall') chars += block.name.length + JSON.stringify(block.arguments).length;
      }
      break;
    case 'bashExecution': chars = message.command.length + message.output.length; break;
    case 'branchSummary':
    case 'compactionSummary': chars = message.summary.length; break;
  }
  return Math.ceil(chars / 4);
}

function buildSessionPath(entries, leafId, byId) {
  const index = byId ?? new Map(entries.map((entry) => [entry.id, entry]));
  if (leafId === null) return [];
  let current = (leafId ? index.get(leafId) : undefined) ?? entries[entries.length - 1];
  const path = [];
  while (current) {
    path.push(current);
    current = current.parentId ? index.get(current.parentId) : undefined;
  }
  return path.reverse();
}

function sessionEntryToContextMessages(entry) {
  if (entry.type === 'message') {
    const message = entry.message;
    if (message.role === 'system' && message.content == null) return [{ ...message, content: '' }];
    if (['user', 'assistant', 'toolResult'].includes(message.role) && message.content == null) {
      return [{ ...message, content: [] }];
    }
    return [message];
  }
  const timestamp = new Date(entry.timestamp).getTime();
  if (entry.type === 'custom_message') {
    return [{ role: 'custom', customType: entry.customType, content: entry.content ?? [],
      display: entry.display, details: entry.details, timestamp }];
  }
  if (entry.type === 'branch_summary' && entry.summary) {
    return [{ role: 'branchSummary', summary: entry.summary, fromId: entry.fromId, timestamp }];
  }
  if (entry.type === 'compaction') {
    const summary = { role: 'compactionSummary', summary: entry.summary, tokensBefore: entry.tokensBefore, timestamp };
    return entry.systemMessage ? [entry.systemMessage, summary] : [summary];
  }
  return [];
}

function contextEntriesForPath(path) {
  const compactionIdx = path.findLastIndex((entry) => entry.type === 'compaction');
  if (compactionIdx < 0) return path;
  const compaction = path[compactionIdx];
  const entries = [compaction];
  let foundFirstKept = false;
  for (let i = 0; i < compactionIdx; i++) {
    const entry = path[i];
    if (entry.id === compaction.firstKeptEntryId) foundFirstKept = true;
    if (foundFirstKept && !(entry.type === 'message' && entry.message.role === 'system')) entries.push(entry);
  }
  return entries.concat(path.slice(compactionIdx + 1));
}

function projectContextEntry(entry, edit) {
  const messages = sessionEntryToContextMessages(entry);
  if (!edit) return messages;
  if (edit.replacement === null) return [];
  return messages.map((message) => {
    if (!['user', 'assistant', 'toolResult', 'custom'].includes(message.role)) return message;
    const content = (message.role === 'assistant' || message.role === 'toolResult') && typeof edit.replacement.content === 'string'
      ? [{ type: 'text', text: edit.replacement.content }]
      : edit.replacement.content;
    return { ...message, content };
  });
}

/** Pi's pure tree projection; does not read, edit, or persist native session state. */
export function buildContextEntries(entries, leafId, byId) {
  return contextEntriesForPath(buildSessionPath(entries, leafId, byId));
}

export function buildSessionProjection(entries, leafId, byId) {
  const path = buildSessionPath(entries, leafId, byId);
  let thinkingLevel = 'off';
  let model = null;
  for (const entry of path) {
    if (entry.type === 'thinking_level_change') thinkingLevel = entry.thinkingLevel;
    else if (entry.type === 'model_change') model = { provider: entry.provider, modelId: entry.modelId };
    else if (entry.type === 'message' && entry.message.role === 'assistant') {
      model = { provider: entry.message.provider, modelId: entry.message.model };
    }
  }
  const contextEntries = contextEntriesForPath(path);
  const latestEdits = new Map();
  for (const entry of contextEntries) {
    if (entry.type === 'context_edit') latestEdits.set(entry.targetId, entry);
  }
  const projectedEntries = contextEntries.map((sourceEntry, index) => ({
    sourceEntry, messages: sourceEntry.type === 'compaction' && index > 0 ? [] : projectContextEntry(sourceEntry, latestEdits.get(sourceEntry.id)),
  }));
  return { entries: projectedEntries, messages: projectedEntries.flatMap(entry => entry.messages), thinkingLevel, model };
}

export function buildSessionContext(entries, leafId, byId) {
  const { messages, thinkingLevel, model } = buildSessionProjection(entries, leafId, byId);
  return { messages, thinkingLevel, model };
}

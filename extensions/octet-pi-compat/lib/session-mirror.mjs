import { contextBytes, historyItems } from './context-limits.mjs';
// Translate host-owned native records; never read/write the Session JSONL here.
import { canonicalToPi } from './provider-context.mjs';
import { bounded, invalid, plainJSON, unsupported } from './errors.mjs';

// Translating walks each message's ancestry, so a host view is translated once
// per namespace and limits. Views only grow by append, so the length detects
// a stale translation. Callers get copies and never share mutable entries.
const translations = new WeakMap();
function cachedTranslation(entries, namespace) {
  if (!Array.isArray(entries)) return translateSessionEntries(entries, namespace);
  const key = `${namespace}\0${historyItems()}\0${contextBytes()}`;
  let byKey = translations.get(entries);
  if (!byKey) translations.set(entries, byKey = new Map());
  let cached = byKey.get(key);
  if (cached?.length !== entries.length) byKey.set(key, cached = { length: entries.length, result: translateSessionEntries(entries, namespace) });
  return cached.result;
}
export const sessionEntryCopies = (entries, namespace) => structuredClone(cachedTranslation(entries, namespace));
export function sessionEntryCopy(entries, namespace, id) {
  const entry = cachedTranslation(entries, namespace).find(candidate => candidate.id === id);
  return entry === undefined ? undefined : structuredClone(entry);
}

export function translateSessionEntries(entries, namespace) {
  if (!Array.isArray(entries) || entries.length > historyItems()) invalid('session entries snapshot');
  const byId = new Map(entries.map(entry => [entry.id, entry]));
  if (byId.size !== entries.length) invalid('duplicate session entry id');
  return entries.map(entry => {
    if (!entry?.value) return plainJSON(entry, 'Pi session entry', contextBytes());
    const id = bounded(entry.id, 'session entry id', 256), parentId = entry.parent;
    if (parentId !== null) bounded(parentId, 'session parent id', 256);
    const base = { id, parentId };
    if (entry.timestamp_unix_ms !== undefined) {
      if (!Number.isSafeInteger(entry.timestamp_unix_ms) || entry.timestamp_unix_ms < 0) invalid('session timestamp');
      base.timestamp = new Date(entry.timestamp_unix_ms).toISOString();
    }
    const setup = namespace && entry.metadata?.extension_metadata?.[namespace];
    if (setup && setup.provenance?.extension === namespace && setup.value?.pi_session_entry) {
      const recorded = plainJSON(setup.value.pi_session_entry, 'setup session entry', 16384);
      return { ...recorded, ...base };
    }
    const custom = entry.metadata?.custom_message;
    if (custom) {
      return { ...base, type: 'custom_message', customType: bounded(custom.custom_type, 'customType', 128),
        content: plainJSON(custom.content, 'custom message content', contextBytes()), display: custom.display,
        ...(Object.hasOwn(custom, 'details') ? { details: plainJSON(custom.details, 'custom message details') } : {}) };
    }
    const own = namespace && entry.metadata?.extension_metadata?.[namespace];
    if (own) {
      if (own.provenance?.extension !== namespace) invalid('session namespace provenance mismatch');
      if (typeof own.value?.entry_type === 'string' && Object.keys(own.value).length === 2 && Object.hasOwn(own.value, 'data')) {
        return { ...base, type: 'custom', customType: own.value.entry_type, data: plainJSON(own.value.data, 'private session entry', 16384) };
      }
    }
    const value = entry.value;
    if (value.type === 'message') {
      // A result's tool name comes from its real ancestry, not another branch
      // with a coincidentally equal provider call id.
      const calls = new Map(), visited = new Set([id]); let cursor = parentId;
      while (cursor !== null && cursor !== undefined) {
        if (visited.has(cursor)) invalid('cyclic session ancestry'); visited.add(cursor);
        const previous = byId.get(cursor); if (!previous) break; // A bounded branch snapshot may begin after a cut.
        for (const part of previous.value?.Assistant?.content || []) if (part.ToolCall && !calls.has(part.ToolCall.id)) calls.set(part.ToolCall.id, part.ToolCall.name);
        cursor = previous.parent;
      }
      const { type, ...canonical } = value;
      const messages = canonicalToPi([canonical], calls);
      if (messages.length !== 1) unsupported('native mixed-message session entry', 'cannot fabricate extra Pi entry identities');
      const message = messages[0];
      const metadata = entry.metadata?.tool_output?.metadata;
      if (metadata && Object.hasOwn(metadata, 'pi_details')) {
        if (message.role !== 'toolResult') unsupported('session Pi tool details', 'native entry does not identify a tool result');
        message.details = plainJSON(metadata.pi_details, 'Pi tool details');
      }
      if (entry.timestamp_unix_ms !== undefined) message.timestamp = entry.timestamp_unix_ms;
      return { ...base, type: 'message', message };
    }
    if (value.type === 'branch_summary') {
      return { ...base, type: 'branch_summary', summary: value.summary,
        fromId: bounded(value.from_entry, 'branch summary source entry', 256),
        ...(value.details ? { details: plainJSON(value.details, 'branch summary details') } : {}) };
    }
    if (value.type === 'compaction') {
      if (value.snapcompact) unsupported('bitmap compaction Pi session mirror');
      return { ...base, type: 'compaction', summary: value.summary, firstKeptEntryId: value.first_kept,
        ...(value.details ? { details: plainJSON(value.details, 'compaction details') } : {}) };
    }
    // Keep real IDs/ancestry for native non-message markers. They are explicitly
    // not Pi custom entries and cannot become extension-private state or roles.
    // Never publish another extension's private data / opaque provider sidecars.
    return { ...base, type: 'octet_native', nativeType: bounded(value.type, 'native entry type', 128) };
  });
}
export function translateHostSnapshot(host, namespace) {
  const translated = { ...host };
  for (const key of ['session_entries', 'session_branch']) if (Object.hasOwn(host, key)) translated[key] = translateSessionEntries(host[key], namespace);
  return translated;
}

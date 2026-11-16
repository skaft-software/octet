import { contextBytes, historyItems } from './context-limits.mjs';
// Translate host-owned native records; never read/write the Session JSONL here.
import { canonicalToPi } from './provider-context.mjs';
import { assistantMetadata } from './model-turns.mjs';
import { assistantObservation } from './run-messages.mjs';
import { bounded, invalid, plainJSON, unsupported } from './errors.mjs';

// Native transport arrays are append-only within a revision lineage. Replacement,
// rewind and fork publications have their own arrays and therefore their own
// weakly held cache. Limits and namespace remain part of the translation identity.
const translations = new WeakMap();
function cachedTranslation(entries, namespace) {
  if (!Array.isArray(entries) || entries.length > historyItems()) invalid('session entries snapshot');
  const key = `${namespace}\0${historyItems()}\0${contextBytes()}`;
  let byKey = translations.get(entries);
  if (!byKey) translations.set(entries, byKey = new Map());
  let cached = byKey.get(key);
  if (cached?.length === entries.length) return cached;
  if (cached && entries.length < cached.length) cached = undefined;
  try {
    if (cached && entries.length > cached.length) {
      for (let i = cached.length; i < entries.length; i++) {
        const entry = entries[i];
        // A branch change needs an ancestry reconstruction, not the old tip's
        // active tool names. Ordinary receipt appends take only the new suffix.
        if (entry.parent !== cached.tip) { cached = undefined; break; }
        if (cached.native.has(entry.id)) invalid('duplicate session entry id');
        const names = resultToolNames(entry, cached.calls);
        const translated = translateEntry(entry, namespace, { names: new Map([[entry.id, names]]), visited: new Set([entry.id]) });
        cached.result.push(translated); cached.byId.set(entry.id, translated);
        cached.native.set(entry.id, entry); cached.tip = entry.id;
        addToolNames(entry, cached.calls);
      }
      if (cached) cached.length = entries.length;
    }
    if (!cached) {
      const result = translateSessionEntries(entries, namespace);
      const native = new Map(entries.map(entry => [entry.id, entry]));
      const calls = new Map(), chain = [];
      let cyclic = false;
      for (let entry = entries.at(-1); entry; entry = native.get(entry.parent)) {
        chain.push(entry);
        // Non-message marker cycles are observable, but not a usable call scope.
        if (chain.length > entries.length) { cyclic = true; break; }
      }
      for (let i = chain.length - 1; i >= 0; i--) addToolNames(chain[i], calls);
      cached = { length: entries.length, result, byId: new Map(result.map(entry => [entry.id, entry])), native, calls, tip: cyclic ? undefined : entries.at(-1)?.id ?? null };
    }
    // At most eight namespace/limit identities per admitted array. Eviction only
    // affects reuse, never the immutable observations already handed to callers.
    if (!byKey.has(key) && byKey.size >= 8) byKey.delete(byKey.keys().next().value);
    byKey.set(key, cached);
    return cached;
  } catch (error) { byKey.delete(key); throw error; }
}
function addToolNames(entry, calls) {
  const local = new Set();
  for (const part of entry.value?.Assistant?.content ?? []) {
    const call = part.ToolCall;
    if (call && !local.has(call.id)) { local.add(call.id); calls.set(call.id, call.name); }
  }
}
function resultToolNames(entry, calls) {
  const names = new Map();
  for (const part of entry.value?.User?.content ?? []) {
    const id = part.ToolResult?.tool_call_id;
    if (id !== undefined && calls.has(id)) names.set(id, calls.get(id));
  }
  return names;
}
function observedEntry(entry) {
  if (entry.type === 'message' && entry.message?.role === 'assistant') {
    entry.message = assistantObservation(entry.message);
  }
  return entry;
}
// Translations are plain JSON data. Clone mutable containers, but share their
// immutable strings: structuredClone duplicates large transcript strings on
// every footer/history read, even though extensions cannot mutate a string.
function copySessionData(value) {
  if (Array.isArray(value)) return value.map(copySessionData);
  if (value === null || typeof value !== 'object') return value;
  const copy = {};
  for (const key of Object.keys(value)) {
    const item = copySessionData(value[key]);
    if (key === '__proto__') Object.defineProperty(copy, key, { value: item, writable: true, enumerable: true, configurable: true });
    else copy[key] = item;
  }
  return copy;
}
export const sessionEntryCopies = (entries, namespace) => copySessionData(cachedTranslation(entries, namespace).result).map(observedEntry);
export function sessionEntryCopy(entries, namespace, id) {
  // Pi packages/coding-agent/src/core/session-manager.ts:1423-1425 uses byId.
  const entry = cachedTranslation(entries, namespace).byId.get(id);
  return entry === undefined ? undefined : observedEntry(copySessionData(entry));
}

// Resolve each result against its actual ancestors in one iterative tree walk.
// The closest call wins, including when another branch reuses its ID. Only
// result IDs are retained, not a complete copy of every ancestor's call map.
function indexToolNames(entries, byId) {
  const children = new Map(), roots = [];
  for (const entry of entries) {
    const parent = entry.parent;
    if (parent === null || parent === undefined || !byId.has(parent)) roots.push(entry);
    else {
      if (!children.has(parent)) children.set(parent, []);
      children.get(parent).push(entry);
    }
  }
  const names = new Map(), visited = new Set(), active = new Map();
  const stack = roots.reverse().map(entry => ({ entry }));
  while (stack.length) {
    const frame = stack.pop();
    if (frame.restore) {
      for (const [id, existed, previous] of frame.restore) {
        if (existed) active.set(id, previous); else active.delete(id);
      }
      continue;
    }
    const entry = frame.entry;
    visited.add(entry.id);
    const user = entry.value?.User?.content;
    if (Array.isArray(user)) for (const part of user) {
      const id = part?.ToolResult?.tool_call_id;
      if (id !== undefined && active.has(id)) {
        if (!names.has(entry.id)) names.set(entry.id, new Map());
        names.get(entry.id).set(id, active.get(id));
      }
    }
    const assistant = entry.value?.Assistant?.content, restore = [], local = new Set();
    if (Array.isArray(assistant)) for (const part of assistant) {
      const call = part?.ToolCall;
      // The old ancestry walk takes the first occurrence within an ancestor.
      if (call && !local.has(call.id)) {
        local.add(call.id);
        restore.push([call.id, active.has(call.id), active.get(call.id)]);
        active.set(call.id, call.name);
      }
    }
    if (restore.length) stack.push({ restore });
    const descendants = children.get(entry.id) ?? [];
    for (let i = descendants.length - 1; i >= 0; i--) stack.push({ entry: descendants[i] });
  }
  return { names, visited };
}

export function translateSessionEntries(entries, namespace) {
  if (!Array.isArray(entries) || entries.length > historyItems()) invalid('session entries snapshot');
  const byId = new Map(entries.map(entry => [entry.id, entry]));
  if (byId.size !== entries.length) invalid('duplicate session entry id');
  const indexed = indexToolNames(entries, byId);
  return entries.map(entry => translateEntry(entry, namespace, indexed));
}
function translateEntry(entry, namespace, indexed) {
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
      const calls = new Map(indexed.names.get(id));
      // With unique IDs and one parent per entry, a rootless component is a
      // cycle or descends from one. Keep the message refusal without walking it
      // again; unrelated native markers retain their existing observation shape.
      if (!indexed.visited.has(id)) invalid('cyclic session ancestry');
      const { type, ...canonical } = value;
      // Session-manager entries are read-only observations, not provider replay.
      // As with model-turn observations, a foreign opaque signature must not make
      // history unreadable or acquire authority to replay that continuation.
      const messages = canonicalToPi([canonical], calls, { observation: true });
      if (messages.length !== 1) unsupported('native mixed-message session entry', 'cannot fabricate extra Pi entry identities');
      const message = messages[0];
      const metadata = entry.metadata?.tool_output?.metadata;
      if (metadata && Object.hasOwn(metadata, 'pi_details')) {
        if (message.role !== 'toolResult') unsupported('session Pi tool details', 'native entry does not identify a tool result');
        message.details = plainJSON(metadata.pi_details, 'Pi tool details');
      }
      if (entry.timestamp_unix_ms !== undefined) message.timestamp = entry.timestamp_unix_ms;
      if (message.role === 'assistant') assistantFacts(message, entry);
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
}
export function translateHostSnapshot(host, namespace) {
  const translated = { ...host };
  for (const key of ['session_entries', 'session_branch']) if (Object.hasOwn(host, key)) translated[key] = translateSessionEntries(host[key], namespace);
  return translated;
}

// Pi assistant messages carry their own usage and stop reason, and extensions
// such as Pi's custom-footer example read them from every branch entry. The
// host attaches the usage ledger's record for each assistant; without one the
// facts stay absent rather than invented, exactly as model-turn observation.
function assistantFacts(message, entry) {
  if (entry.assistant_metadata != null) assistantMetadata(message, entry, entry.assistant_metadata);
}

// Command-context replacements keep the command RPC live, not its old Session.
import { dirname } from 'node:path';
import { bounded, fields, invalid, ownerKey, plainJSON, rpcError, strict, unsupported } from './errors.mjs';
import { entryPayload } from './session-leaf.mjs';
import { sessionEntryCopies, sessionEntryCopy } from './session-mirror.mjs';
import { piToCanonical } from './provider-context.mjs';
import { customMessage } from './custom-messages.mjs';

// Creation identity/provenance comes from the durable native header. Historical
// files without a header return undefined rather than fabricated timestamps.
export function sessionHeader(host) {
  if (host.session_entries === null || host.session_branch === null) unsupported('ctx.sessionManager.getHeader', 'native session view unavailable');
  const header = host.session_header;
  if (header == null) return undefined;
  bounded(header.id, 'session header id', 256); bounded(header.cwd, 'session header cwd', 4096);
  if (!Number.isSafeInteger(header.timestamp_unix_ms) || header.timestamp_unix_ms < 0) invalid('session header timestamp');
  return { type: 'session', version: 3, id: header.id, cwd: header.cwd,
    timestamp: new Date(header.timestamp_unix_ms).toISOString(),
    ...(header.parent_session === null ? {} : { parentSession: bounded(header.parent_session, 'parent session', 4096) }) };
}

// This facade never opens a file or invents an entry ID. Synchronous writes
// wait for the foreground lifecycle driver to commit against the real Session.
function setupManager(runtime, store) {
  let writable = true;
  const live = () => { runtime.assertOwner(store); store.controller.signal.throwIfAborted(); };
  // Session facts come from whichever transport delivers them; owner, cwd and
  // workspace stays host-owned and reading them grants no authority.
  const facts = () => { live(); return runtime.sessionFacts(store); };
  const entries = () => sessionEntryCopies(facts().session_entries, runtime.namespace);
  const entry = id => sessionEntryCopy(facts().session_entries, runtime.namespace, id);
  const branch = (id = facts().session_leaf_id) => {
    const all = entries(), byId = new Map(all.map(entry => [entry.id, entry])), path = [], seen = new Set();
    while (id !== null) {
      if (seen.has(id)) invalid('cyclic session branch'); seen.add(id);
      const entry = byId.get(id); if (!entry) invalid('unknown session branch entry');
      path.unshift(entry); id = entry.parentId;
    }
    return path;
  };
  const apply = receipt => {
    fields(receipt, ['entry_id', 'context'], 'session setup receipt');
    if (ownerKey(receipt.context?.resource_owner) !== ownerKey(store.state.owner)) invalid('session setup receipt owner');
    runtime.bind({ context: receipt.context }, store);
    return receipt.entry_id;
  };
  const mutate = mutation => {
    live();
    if (!writable) throw rpcError(-32002, 'not_foreground_owner session setup completed');
    if (!store.live || runtime.active.get(store.id)?.controller !== store.controller) throw rpcError(-32002, 'session setup requires its live command');
    return apply(runtime.transport.requestSync('session/setup', {
      parent_request_id: store.id, resource_owner: store.state.owner, mutation,
    }, { parent: store.id, signal: store.controller.signal, onCancel: () => store.controller.abort(rpcError(-32800, 'request cancelled')) }));
  };
  const append = value => {
    const entry = entryPayload('session-setup', value);
    const id = mutate({ kind: 'append', entry }); bounded(id, 'setup entry id', 256); return id;
  };
  const manager = strict({
    getHeader: () => sessionHeader(facts()),
    getSessionId: () => facts().session_id,
    getSessionFile: () => facts().session_file,
    getSessionDir: () => dirname(facts().session_file),
    getCwd: () => { live(); return store.state.workspace; },
    isPersisted: () => facts().session_file !== null,
    getEntries: entries, getBranch: branch,
    getEntry: id => entry(id),
    getLeafId: () => facts().session_leaf_id,
    getLeafEntry: () => entry(facts().session_leaf_id),
    getChildren: id => entries().filter(entry => entry.parentId === id),
    getLabel: id => facts().session_labels?.[id],
    getSessionName: () => {
      const latest = entries().findLast(entry => entry.type === 'session_info');
      return latest ? latest.name.trim() || undefined : facts().session_name ?? undefined;
    },
    appendMessage(message) {
      const clean = plainJSON(message, 'setup message', 16384), [canonical] = piToCanonical([clean]);
      return append({ type: 'message', message: clean, canonical_message: canonical,
        ...(clean.role === 'custom' ? { custom_message: customMessage({ customType: clean.customType, content: clean.content, display: clean.display, ...(clean.details === undefined ? {} : { details: clean.details }) }) } : {}) });
    },
    appendCustomEntry(customType, data) {
      bounded(customType, 'custom type', 128);
      return append({ type: 'custom', customType, ...(data === undefined ? {} : { data }) });
    },
    appendThinkingLevelChange(thinkingLevel) {
      bounded(thinkingLevel, 'thinking level', 128); return append({ type: 'thinking_level_change', thinkingLevel });
    },
    appendModelChange(provider, modelId) {
      bounded(provider, 'provider', 256); bounded(modelId, 'model id', 256); return append({ type: 'model_change', provider, modelId });
    },
    appendCustomMessageEntry(customType, content, display, details) {
      if (typeof display !== 'boolean') invalid('custom message display');
      const value = customMessage({ customType, content, display, ...(details === undefined ? {} : { details }) });
      return append({ type: 'custom_message', customType, content: value.content, display,
        ...(details === undefined ? {} : { details: value.details }), custom_message: value });
    },
    appendSessionInfo(name) {
      bounded(name, 'session name', 4096, { controls: true });
      return append({ type: 'session_info', name: name.replace(/[\r\n]+/g, ' ').trim() });
    },
    appendLabelChange(targetId, label) {
      bounded(targetId, 'label target id', 256); if (label !== undefined) bounded(label, 'entry label', 128);
      return append({ type: 'label', targetId, ...(label === undefined ? {} : { label }) });
    },
    branch(entryId) { bounded(entryId, 'branch entry id', 256); mutate({ kind: 'branch', entry_id: entryId }); },
    resetLeaf() { mutate({ kind: 'branch', entry_id: null }); },
  }, 'newSession setup SessionManager');
  return { manager, async complete() {
    writable = false;
    // Async completion lets Node dispatch the real session_start hooks; a sync
    // wait here would deadlock the main thread that runs those callbacks.
    const result = await runtime.hostCall('session/setup', { resource_owner: store.state.owner, mutation: { kind: 'complete' } }, store);
    apply(result);
  } };
}

export function sessionMethods(runtime, store, createContext) {
  const command = name => {
    if (store.method !== 'command/execute') unsupported(`ctx.${name}`, 'command context only');
    runtime.assertOwner(store);
  };
  const replace = async (name, method, params, withSession, setup) => {
    command(name);
    if (withSession !== undefined && typeof withSession !== 'function') invalid(`${name} withSession`);
    runtime.require('session_control_v1');
    const bindSetup = setup ? runtime.prepareSessionReplacement(store) : undefined;
    const result = await runtime.track(runtime.hostCall(method, {
      resource_owner: store.state.owner, ...params,
    }, store), store);
    fields(result, ['session_id', 'cancelled', ...(setup ? ['context'] : [])], `${name} receipt`);
    if (result.cancelled !== undefined && typeof result.cancelled !== 'boolean') invalid(`${name} cancelled`);
    if (result.cancelled) return { cancelled: true };
    bounded(result.session_id, 'replacement session id', 256);
    // Setup binds from an authenticated native creation receipt, before start
    // hooks; ordinary replacements await the real lifecycle binding as before.
    const fresh = setup ? bindSetup(result.context, result.session_id)
      : { ...store, state: await runtime.foregroundFor(result.session_id, store.controller.signal) };
    if (setup) {
      const writable = setupManager(runtime, fresh);
      let failed = false, error;
      try { await runtime.scope.run(fresh, () => setup(writable.manager)); }
      catch (cause) { failed = true; error = cause; }
      // A throwing setup retains its already committed native writes, starts
      // the new session, and still reports failure instead of false success.
      try { await writable.complete(); } catch (cause) { if (!failed) throw cause; }
      if (failed) throw error;
    }
    if (withSession) await runtime.scope.run(fresh, () => withSession(createContext(runtime, fresh, true)));
    return { cancelled: false };
  };
  return {
    newSession(options = {}) {
      fields(options, ['parentSession', 'setup', 'withSession'], 'newSession options');
      if (options.parentSession !== undefined) bounded(options.parentSession, 'parent session', 4096);
      if (options.setup !== undefined && typeof options.setup !== 'function') invalid('newSession setup');
      return replace('newSession', 'session/create', {
        ...(options.parentSession === undefined ? {} : { parent_session: options.parentSession }),
        ...(options.setup === undefined ? {} : { setup: true }),
      }, options.withSession, options.setup);
    },
    fork(entryId, options = {}) {
      fields(options, ['position', 'withSession'], 'fork options'); bounded(entryId, 'entry id', 256);
      if (options.position !== undefined && !['before', 'at'].includes(options.position)) invalid('fork position');
      return replace('fork', 'session/fork', { entry_id: entryId, ...(options.position ? { position: options.position } : {}) }, options.withSession);
    },
    switchSession(sessionPath, options = {}) {
      fields(options, ['withSession'], 'switchSession options'); bounded(sessionPath, 'session path', 4096);
      return replace('switchSession', 'session/switch', { session_id: sessionPath.split(/[\\/]/).pop().replace(/\.[^.]*$/, '') }, options.withSession);
    },
    reload() {
      command('reload'); runtime.require('session_control_v1');
      return runtime.track(runtime.hostCall('session/reload', { resource_owner: store.state.owner }, store), store).then(() => undefined);
    },
  };
}

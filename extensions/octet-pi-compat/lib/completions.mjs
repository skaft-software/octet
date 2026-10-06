import { fields, invalid, rpcError, unsupported } from './errors.mjs';
import { applyPiCompletion, autocompleteSnapshot, completionResponse, completionText, piPosition } from './autocomplete-edits.mjs';
import { fuzzyFilter } from '../node_modules/@earendil-works/pi-tui/dist/fuzzy.js';

// Command/attachment parsing adapted from Pi 1.0.2 (cd32f7725fdbddbaecdff5b1e68491563394e0ca),
// packages/tui/src/{autocomplete,utils}.ts. Copyright (c) 2025 Mario Zechner.
// MIT, see ../LICENSE.pi. No filesystem/path completion or Pi runtime is started.
const delimiters = new Set([' ', '\t', '"', "'", '=']);
const wrappers = { '(': ')', '[': ']', '{': '}', '<': '>', '`': '`' };
const cjk = /[\p{Script_Extensions=Han}\p{Script_Extensions=Hiragana}\p{Script_Extensions=Katakana}\p{Script_Extensions=Hangul}\p{Script_Extensions=Bopomofo}]/u;
const separator = new RegExp(`(?:\\s|(?=\\p{Punctuation})${cjk.source}|[，．：；！？（）［］｛｝“”‘’…—])`, 'u');
const boundary = new RegExp(`(?:^|${separator.source})$`, 'u');
function atAttachment(text) {
  let quoted = false, quoteStart = -1;
  for (let i = 0; i < text.length; i++) if (text[i] === '"') {
    quoted = !quoted;
    if (quoted) quoteStart = i;
  }
  if (quoted && quoteStart > 0 && text[quoteStart - 1] === '@') {
    let start = quoteStart - 1;
    while (start > 0 && wrappers[text[start - 1]]) start--;
    if (delimiters.has(text[start - 1]) || boundary.test(text.slice(0, start))) return true;
  }
  let last = -1, index = 0;
  for (const char of text) {
    index += char.length;
    if (delimiters.has(char) || separator.test(char)) last = index - 1;
  }
  let token = last === -1 ? text : text.slice(last + 1);
  while (token.length && wrappers[token[0]] && !token.includes(wrappers[token[0]], 1)) token = token.slice(1);
  return token.startsWith('@');
}

/** Translate a native UTF-8 byte cursor; keep Pi's complete raw argument prefix. */
export function commandArgumentRequest(params) {
  const snapshot = autocompleteSnapshot(params);
  const line = snapshot.lines[snapshot.cursorLine].slice(0, snapshot.cursorCol);
  const command = line.trimStart(), space = command.indexOf(' ');
  if (!command.startsWith('/') || space < 0 || atAttachment(line)) return null;
  return { name: command.slice(1, space), prefix: command.slice(space + 1),
    afterCursor: snapshot.text.slice(snapshot.index) };
}

async function cancellable(promise, signal) {
  if (!signal) return promise;
  signal.throwIfAborted();
  let abort;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      abort = () => reject(signal.reason);
      signal.addEventListener('abort', abort, { once: true });
    })]);
  } finally { signal.removeEventListener('abort', abort); }
}

const chains = new WeakMap();
const activeQueries = new WeakMap();

function baseProvider(runtime) {
  return {
    async getSuggestions(lines, cursorLine, cursorCol, options = {}) {
      const { text, index } = piPosition(lines, cursorLine, cursorCol);
      if (options.force) return null; // Filesystem completion remains the native fallback.
      options.signal?.throwIfAborted();
      const line = lines[cursorLine].slice(0, cursorCol), commandText = line.trimStart();
      if (!atAttachment(line) && commandText.startsWith('/') && !commandText.includes(' ')) {
        const items = [...runtime.commands].map(([name, { definition }]) => ({
          name, label: name, description: definition.description,
        }));
        const prefix = commandText.slice(1);
        const bare = fuzzyFilter(items, prefix, item => item.name.startsWith('skill:') ? item.name.slice(6) : item.name);
        const matched = new Set(bare);
        const full = fuzzyFilter(items.filter(item => item.name.startsWith('skill:') && !matched.has(item)), prefix, item => item.name);
        const matches = [...bare, ...full].map(item => ({ value: item.name, label: item.label,
          ...(item.description ? { description: item.description } : {}) }));
        return matches.length ? { prefix: commandText, items: matches } : null;
      }
      const query = commandArgumentRequest({ text, cursor: Buffer.byteLength(text.slice(0, index)), revision: 0 });
      const command = query && runtime.commands.get(query.name);
      if (!command?.definition.getArgumentCompletions) return null;
      const store = runtime.scope.getStore();
      // Keep the invocation alive until the actual callback settles, including
      // cancellation. A signal is not proof that extension JS has stopped.
      const result = await runtime.scope.run({ ...store, factory: command.factory },
        () => command.definition.getArgumentCompletions(query.prefix));
      options.signal?.throwIfAborted();
      if (store?.state) runtime.assertOwner(store);
      // This is the pinned Pi command-callback policy, not the provider-result grammar.
      return Array.isArray(result) && result.length ? { prefix: query.prefix, items: result } : null;
    },
    applyCompletion: applyPiCompletion,
    shouldTriggerFileCompletion(lines, cursorLine, cursorCol) {
      piPosition(lines, cursorLine, cursorCol);
      const text = lines[cursorLine].slice(0, cursorCol).trim();
      return !(text.startsWith('/') && !text.includes(' '));
    },
  };
}

function scopedProvider(runtime, state, factory, provider) {
  if (!provider || typeof provider.getSuggestions !== 'function' || typeof provider.applyCompletion !== 'function') invalid('autocomplete provider requires getSuggestions and applyCompletion');
  if (provider.shouldTriggerFileCompletion !== undefined && typeof provider.shouldTriggerFileCompletion !== 'function') invalid('autocomplete shouldTriggerFileCompletion');
  const triggers = provider.triggerCharacters;
  if (triggers !== undefined) {
    if (!Array.isArray(triggers) || triggers.length > 32) invalid('autocomplete triggerCharacters');
    for (const trigger of triggers) {
      completionText(trigger, 'autocomplete trigger character');
      if ([...trigger].length !== 1) invalid('autocomplete trigger character must be one scalar');
    }
  }
  const call = (method, args) => {
    runtime.assertOwner({ state });
    const current = runtime.scope.getStore();
    if (!current) unsupported('autocomplete provider callback', 'a real request or component context is required');
    return runtime.scope.run({ ...current, state, factory }, () => provider[method](...args));
  };
  return {
    ...(triggers === undefined ? {} : { triggerCharacters: [...triggers] }),
    getSuggestions: (...args) => call('getSuggestions', args),
    applyCompletion: (...args) => call('applyCompletion', args),
    ...(provider.shouldTriggerFileCompletion === undefined ? {} : {
      shouldTriggerFileCompletion(...args) {
        const result = call('shouldTriggerFileCompletion', args);
        if (typeof result !== 'boolean') invalid('autocomplete shouldTriggerFileCompletion must return boolean');
        return result;
      },
    }),
  };
}

/** The one process-owned completion chain. It must exist before the initialize
 * reply write can settle: the host may read that line and query completions
 * immediately, and those queries await this promise. The registration request
 * itself still follows the reply, because the host resolves the negotiated
 * `autocomplete` feature from it.
 */
export function ensureCompletionChain(runtime) {
  if (!runtime.autocompleteRegistration) {
    runtime.autocompleteRegistration = new Promise((resolve, reject) => { runtime.autocompleteSettle = { resolve, reject }; });
    // A chain nobody queries yet must not surface as an unhandled rejection;
    // waiting queries still observe the real failure.
    runtime.autocompleteRegistration.catch(() => {});
  }
  return runtime.autocompleteRegistration;
}

/** Start the chain once the initialize reply (or a dynamic provider) makes it
 * legal. `contribution` covers a component-only provider in a command-less
 * process. Returns the chain, or null when this process contributes nothing.
 */
export function startCompletionChain(runtime, contribution = false) {
  if (runtime.autocompleteStarted || !runtime.features.has('autocomplete') || !(contribution || runtime.commands.size)) return runtime.autocompleteRegistration;
  runtime.autocompleteStarted = true;
  const chain = ensureCompletionChain(runtime);
  const request = runtime.transport.request('ui/autocomplete/register', { revision: 1 }).then(result => {
    fields(result, ['accepted'], 'autocomplete registration acknowledgement');
    if (typeof result.accepted !== 'boolean') invalid('autocomplete registration accepted must be boolean');
    if (!result.accepted) unsupported('command completions', 'host refused autocomplete registration');
    runtime.autocompleteSettle?.resolve(result);
    runtime.autocompleteSettle = null;
  });
  // One failure path for a rejected transport and for a refused or malformed
  // admission: waiting queries see it, and the process reports it once.
  request.catch(error => {
    runtime.autocompleteSettle?.reject(error);
    runtime.autocompleteSettle = null;
    runtime.backgroundError(error);
  });
  return chain;
}

/** API glue: ui.addAutocompleteProvider(factory) calls this with its actual store. */
export function addAutocompleteProvider(runtime, store, factory) {
  runtime.require('autocomplete'); runtime.require('autocomplete_edit_v1'); runtime.assertOwner(store);
  store.controller.signal.throwIfAborted();
  if (typeof factory !== 'function') invalid('autocomplete provider factory');
  const entries = [...(chains.get(store.state)?.entries || []), { factory, ownerFactory: store.factory }];
  if (entries.length > 32) throw rpcError(-32602, 'bounds_exceeded autocomplete providers');
  let provider = baseProvider(runtime);
  const triggers = [];
  // Pi rebuilds the complete chain in registration order on each addition.
  for (const entry of entries) {
    const next = runtime.scope.run({ ...store, factory: entry.ownerFactory }, () => entry.factory(provider));
    provider = scopedProvider(runtime, store.state, entry.ownerFactory, next);
    triggers.push(...(provider.triggerCharacters || []));
  }
  if (triggers.length) provider.triggerCharacters = [...new Set(triggers)];
  chains.set(store.state, { entries, provider });
  const editor = runtime.ui.activeEditor(store);
  if (editor?.component) runtime.scope.run(editor.store, () => editor.component.setAutocompleteProvider?.(getAutocompleteProvider(runtime, editor.store)));
  if (!runtime.autocompleteStarted) {
    // A dynamic provider may be the first completion contribution. Initialization
    // has already settled; this remains a process-owned registration, never a fake parent.
    startCompletionChain(runtime, true);
  }
  runtime.track(runtime.autocompleteRegistration, store);
}

/** Component-only Pi interface; no force/trigger fields are invented on the native wire. */
export function getAutocompleteProvider(runtime, store) {
  runtime.assertOwner(store);
  const state = store.state, chain = chains.get(state), provider = chain?.provider ?? baseProvider(runtime);
  const live = () => {
    runtime.assertOwner(store);
    store.controller.signal.throwIfAborted();
    if (store.surface?.closed) throw rpcError(-32800, 'autocomplete editor retired');
    if (chains.get(state) !== chain) throw rpcError(-32800, 'autocomplete provider chain replaced');
  };
  const call = (method, args) => { live(); return runtime.scope.run(store, () => provider[method](...args)); };
  return {
    ...(provider.triggerCharacters === undefined ? {} : { triggerCharacters: [...provider.triggerCharacters] }),
    async getSuggestions(lines, cursorLine, cursorCol, options = {}) {
      const controller = new AbortController();
      const signal = AbortSignal.any([controller.signal, store.controller.signal, ...(options.signal ? [options.signal] : [])]);
      let active = activeQueries.get(state);
      if (!active) { active = new Set(); activeQueries.set(state, active); }
      active.add(controller);
      try {
        live(); signal.throwIfAborted();
        // Editor requests have no new native parent. Preserve the real surface
        // origin, but fence retained API effects with this query's cancellation.
        const scope = { ...store, controller: { signal, abort: reason => controller.abort(reason) } };
        const result = await runtime.scope.run(scope, () => provider.getSuggestions(lines, cursorLine, cursorCol, { ...options, signal }));
        live(); signal.throwIfAborted(); return result;
      } catch (error) {
        // Pi's editor does not catch provider rejections. Expected cancellation
        // and retirement yield no menu; actual callback failures are reported.
        if (!signal.aborted && state.alive && runtime.foreground === state
            && !store.surface?.closed && chains.get(state) === chain) runtime.backgroundError(error);
        return null;
      } finally { activeQueries.get(state)?.delete(controller); }
    },
    applyCompletion: (...args) => call('applyCompletion', args),
    ...(provider.shouldTriggerFileCompletion === undefined ? {} : {
      shouldTriggerFileCompletion: (...args) => call('shouldTriggerFileCompletion', args),
    }),
  };
}

/** Runtime.retire(state) must call this; it changes no editor/remote-UI lifetime. */
export function retireAutocomplete(state) {
  chains.delete(state);
  for (const controller of activeQueries.get(state) || []) controller.abort(rpcError(-32800, 'autocomplete owner retired'));
  activeQueries.delete(state);
}

export async function commandCompletions(runtime, params, store) {
  runtime.require('autocomplete');
  // The chain is armed before initialize settles; a query that still finds none
  // starts it here, which stays after the reply on the wire.
  const registration = runtime.autocompleteRegistration ?? startCompletionChain(runtime);
  if (!registration) unsupported('command completions', 'no registered completion chain');
  const snapshot = autocompleteSnapshot(params);
  const state = runtime.foreground, chain = state && chains.get(state);
  const provider = chain?.provider ?? baseProvider(runtime);
  if (state) {
    runtime.assertOwner({ state });
    let active = activeQueries.get(state);
    if (!active) { active = new Set(); activeQueries.set(state, active); }
    active.add(store.controller);
  }
  const live = () => {
    store.controller.signal.throwIfAborted();
    if (state) {
      runtime.assertOwner({ state });
      if (chains.get(state) !== chain) throw rpcError(-32800, 'autocomplete provider chain replaced');
    }
  };
  try {
    await cancellable(registration, store.controller.signal); live();
    const scope = { ...store, ...(state ? { state } : {}) };
    // Native requests do not carry natural/forced trigger information. In
    // particular, do not reinterpret every native query as Pi's forced-file Tab.
    const suggestions = await runtime.scope.run(scope, () => provider.getSuggestions(
      [...snapshot.lines], snapshot.cursorLine, snapshot.cursorCol, { signal: store.controller.signal }));
    live();
    const response = runtime.scope.run(scope, () => completionResponse(snapshot, suggestions, provider, runtime.features.has('autocomplete_edit_v1')));
    live(); return response;
  } finally { if (state) activeQueries.get(state)?.delete(store.controller); }
}

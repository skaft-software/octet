// Pi 1.0.2 public renderer adapters. Semantics follow extensions/runner.ts and
// interactive/components/{custom-message,custom-entry,markdown-transform}.ts.
// Copyright (c) 2025 Mario Zechner; MIT, see ../LICENSE.pi.
import { bounded, fields, invalid, plainJSON, rpcError } from './errors.mjs';
import { safeLines } from './remote-ui.mjs';
import { theme } from './theme.mjs';
import { Box } from '../node_modules/@earendil-works/pi-tui/dist/components/box.js';
import { Text } from '../node_modules/@earendil-works/pi-tui/dist/components/text.js';
import { truncateToWidth } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';

const MAX_COMPONENTS = 128;
function markdown(text) {
  bounded(text, 'render Markdown', 262144, { controls: true });
  // Preserve approved SGR. Validate source lines without imposing the frame's
  // 256-row limit on Markdown that the existing native renderer will wrap.
  for (const line of text.split('\n')) safeLines([line.replace(/\t/g, ' ')]);
  return text;
}
function boolean(value, label) { if (typeof value !== 'boolean') invalid(label); return value; }
function dispose(entry) {
  for (const component of new Set([entry?.component, entry?.callComponent, entry?.resultComponent])) component?.dispose?.();
}

export class TranscriptRenderers {
  constructor(runtime) { this.runtime = runtime; this.factories = new Map(); this.components = new WeakMap(); }
  api(factory) {
    const register = (kind, customType, callback) => {
      if (this.runtime.stopping) throw rpcError(-32002, 'renderer runtime is retired');
      this.runtime.scope.getStore()?.controller.signal.throwIfAborted();
      if (this.runtime.loaded) this.runtime.current(factory);
      // Late terminal registrations need the same negotiated consumer as
      // load-time registrations; otherwise they would be silently ignored.
      if (this.runtime.initialized && this.runtime.features.has('remote_ui')) this.runtime.require('transcript_render_v1');
      if (typeof callback !== 'function') invalid(`${kind} renderer must be a function`);
      let entries = this.factories.get(factory);
      if (!entries) { entries = { message: new Map(), entry: new Map(), markdown: undefined, tool: [] }; this.factories.set(factory, entries); }
      if (kind === 'tool') {
        if (entries.tool.length >= 1024) invalid('tool renderer registration limit');
        entries.tool.push(callback);
      } else if (kind === 'markdown') entries.markdown = callback; // Last registration per extension wins.
      else {
        bounded(customType, 'renderer customType', 128);
        if (!entries[kind].has(customType) && entries.message.size + entries.entry.size >= 1024) invalid('renderer registration limit');
        entries[kind].set(customType, callback);
      }
    };
    return {
      registerToolRenderer: callback => register('tool', undefined, callback),
      registerMessageRenderer: (customType, callback) => register('message', customType, callback),
      registerEntryRenderer: (customType, callback) => register('entry', customType, callback),
      registerMarkdownTransformer: callback => register('markdown', undefined, callback),
    };
  }
  ordered() { return [...this.factories].sort(([a], [b]) => a - b); }
  metadata() {
    return this.ordered().map(([factory, entries]) => ({ factory,
      messages: [...entries.message.keys()], entries: [...entries.entry.keys()],
      markdown: Boolean(entries.markdown), tools: entries.tool.length }));
  }
  cache(store) {
    let cache = this.components.get(store.state);
    if (!cache) { cache = new Map(); this.components.set(store.state, cache); }
    return cache;
  }
  remember(cache, key, entry) {
    cache.delete(key); cache.set(key, entry);
    if (cache.size > MAX_COMPONENTS) { const oldest = cache.keys().next().value; dispose(cache.get(oldest)); cache.delete(oldest); }
  }
  resolveTool(name, store) {
    bounded(name, 'renderer tool name', 128);
    const resolvers = this.ordered().flatMap(([factory, entries]) => entries.tool.map(callback => ({ factory, callback })));
    const base = this.runtime.tools.get(name);
    let factory = base?.factory;
    const next = index => {
      if (index === resolvers.length) return base?.definition;
      const resolver = resolvers[index];
      const result = this.call(store, resolver.factory, () => resolver.callback(name, () => next(index + 1)));
      // A resolver can deliberately suppress the remaining chain.
      if (result !== undefined) factory = resolver.factory;
      return result;
    };
    const definition = next(0);
    if (definition === undefined) return undefined;
    if (!definition || typeof definition !== 'object') invalid('tool renderer resolver result');
    for (const key of ['renderCall', 'renderResult']) if (definition[key] !== undefined && typeof definition[key] !== 'function') invalid(`tool renderer ${key}`);
    if (definition.renderShell !== undefined && !['default', 'self'].includes(definition.renderShell)) invalid('tool renderShell');
    return { definition, factory };
  }
  tool(params, store) {
    const request = params.render;
    fields(request, ['kind', 'name', 'arguments', 'result', 'expanded', 'is_partial', 'is_error', 'execution_started', 'args_complete', 'show_images'], 'tool render content');
    const selected = this.resolveTool(request.name, store);
    if (!selected) return { registered: false, lines: null, markdown: null, render_shell: null };
    const { definition, factory } = selected;
    const cache = this.cache(store), key = `tool:${params.source_id}`;
    let cached = cache.get(key);
    if (!cached || cached.callRenderer !== definition.renderCall || cached.resultRenderer !== definition.renderResult) {
      dispose(cached);
      cached = { state: {}, callRenderer: definition.renderCall, resultRenderer: definition.renderResult };
    }
    const context = { args: plainJSON(request.arguments, 'renderer arguments', 262144), toolCallId: params.source_id,
      cwd: store.state.workspace, state: cached.state, invalidate() { cached.callComponent?.invalidate?.(); cached.resultComponent?.invalidate?.(); },
      executionStarted: boolean(request.execution_started, 'executionStarted'), argsComplete: boolean(request.args_complete, 'argsComplete'),
      isPartial: boolean(request.is_partial, 'isPartial'), expanded: boolean(request.expanded, 'expanded'),
      showImages: boolean(request.show_images, 'showImages'), isError: boolean(request.is_error, 'isError') };
    const replace = (slot, callback) => {
      const previous = cached[slot];
      const component = this.call(store, factory, () => callback({ ...context, lastComponent: previous }));
      if (!component || typeof component.render !== 'function') invalid('tool renderer must return a Component');
      if (component !== previous) previous?.dispose?.();
      cached[slot] = component;
    };
    // Remember before callback invocation: any component produced by a later
    // failing slot is still owned and disposed on retirement/eviction.
    this.remember(cache, key, cached);
    if (definition.renderCall) replace('callComponent', context => definition.renderCall(context.args, theme, context));
    if (definition.renderResult && request.result !== null) {
      const result = plainJSON(request.result, 'renderer tool result', 524288);
      replace('resultComponent', context => definition.renderResult(result,
        { expanded: context.expanded, isPartial: context.isPartial }, theme, context));
    }
    this.remember(cache, key, cached);
    const lines = [cached.callComponent, request.result === null ? null : cached.resultComponent].filter(Boolean)
      .flatMap(component => this.call(store, factory, () => safeLines(component.render(params.width))))
      .map(line => truncateToWidth(line, params.width, ''));
    return { registered: true, lines: safeLines(lines), markdown: null, render_shell: definition.renderShell ?? 'default' };
  }
  live(store) { store.controller.signal.throwIfAborted(); this.runtime.assertOwner(store); }
  call(store, factory, callback) {
    this.live(store);
    const result = this.runtime.scope.run({ ...store, factory }, callback);
    try { this.live(store); }
    catch (error) { result?.dispose?.(); throw error; }
    return result;
  }
  render(params, store) {
    this.runtime.require('transcript_render_v1'); this.runtime.require('remote_ui'); this.live(store);
    fields(params, ['source_id', 'width', 'render', 'context'], 'transcript/render');
    bounded(params.source_id, 'render source identity', 256);
    if (!params.source_id || !Number.isInteger(params.width) || params.width <= 0 || params.width > 65535) invalid('transcript render geometry/identity');
    const request = params.render;
    if (request?.kind === 'tool') return this.tool(params, store);
    fields(request, request?.kind === 'markdown' ? ['kind', 'text', 'message_type', 'is_streaming']
      : request?.kind === 'message' ? ['kind', 'message', 'expanded', 'output_pad'] : ['kind', 'entry', 'expanded'], 'transcript render content');
    if (request.kind === 'markdown') {
      if (!['user', 'assistant', 'assistant-thinking'].includes(request.message_type)) invalid('Markdown message type');
      const context = { messageType: request.message_type, isStreaming: boolean(request.is_streaming, 'Markdown streaming'), availableWidth: params.width };
      let text = markdown(request.text), registered = false;
      for (const [factory, entries] of this.ordered()) {
        if (!entries.markdown) continue;
        registered = true;
        let result;
        try { result = this.call(store, factory, () => entries.markdown(text, context)); }
        catch (error) { this.live(store); continue; } // Pi keeps current Markdown after a callback exception.
        if (typeof result === 'string') text = markdown(result);
      }
      return { registered, lines: null, markdown: text };
    }
    if (!['message', 'entry'].includes(request.kind)) invalid('transcript render kind');
    const value = plainJSON(request[request.kind], `${request.kind} render input`, 786432);
    bounded(value.customType, 'renderer customType', 128);
    const options = { expanded: boolean(request.expanded, 'renderer expanded') };
    if (request.kind === 'message') {
      if (!Number.isInteger(request.output_pad) || request.output_pad < 0 || request.output_pad > 65535) invalid('message outputPad');
      options.outputPad = request.output_pad;
    }
    const selected = this.ordered().find(([, entries]) => entries[request.kind].has(value.customType));
    // First registered renderer wins across extensions, even when it returns undefined.
    if (!selected) return { registered: false, lines: null, markdown: null };
    const [factory, entries] = selected, callback = entries[request.kind].get(value.customType);
    const cache = this.cache(store);
    const key = `${request.kind}:${params.source_id}`, signature = JSON.stringify([value, options]);
    let cached = cache.get(key);
    if (!cached || cached.callback !== callback || cached.signature !== signature || cached.theme !== theme) {
      dispose(cached); cache.delete(key);
      let component;
      try { component = this.call(store, factory, () => callback(value, options, theme)); }
      catch (error) {
        this.live(store);
        if (request.kind === 'entry') {
          component = new Box(1, 1, text => theme.bg('customMessageBg', text));
          component.addChild(new Text(theme.fg('error', `[${value.customType}] renderer failed: ${error instanceof Error ? error.message : String(error)}`), 0, 0));
        }
      }
      if (component !== undefined && component !== null && typeof component.render !== 'function') invalid('renderer must return a Component or undefined');
      cached = { component, factory, callback, signature, theme };
    }
    this.remember(cache, key, cached);
    if (!cached.component) return { registered: true, lines: null, markdown: null };
    const lines = this.call(store, cached.factory, () => safeLines(cached.component.render(params.width)))
      .map(line => truncateToWidth(line, params.width, ''));
    return { registered: true, lines: safeLines(lines), markdown: null };
  }
  retire(state) {
    const cache = this.components.get(state);
    if (cache) for (const entry of cache.values()) dispose(entry);
    this.components.delete(state);
  }
}

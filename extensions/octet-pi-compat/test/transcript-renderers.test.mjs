import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { TranscriptRenderers } from '../lib/transcript-renderers.mjs';
import { safeLines } from '../lib/remote-ui.mjs';
import { Text } from '../node_modules/@earendil-works/pi-tui/dist/components/text.js';
import { visibleWidth } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

// Real adapter subprocess/dispatch integration, still a synthetic host. This
// cannot qualify Rust painting, persistence, resize or owner publication.
test('runtime exposes public registrations and routes owned transcript/render requests', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'octet-pi-transcript-'));
  t.after(() => rmSync(directory, {recursive: true, force: true}));
  const factory = join(directory, 'render.ts');
  writeFileSync(factory, `import { Text } from '@earendil-works/pi-tui';
export default pi => pi.registerCommand('install', {handler: () => {
  pi.registerMessageRenderer('notice', (message, options, theme) => new Text(theme.fg('success', message.details.actual + ':' + options.outputPad), 0, 0));
  pi.registerEntryRenderer('saved', entry => new Text(entry.id + ':' + entry.data.actual, 0, 0));
  pi.registerMarkdownTransformer((text, context) => text + ':' + context.availableWidth);
  pi.registerToolRenderer((name, next) => next() ?? {renderShell: 'self', renderCall: () => new Text(name, 0, 0)});
}});`);
  const peer = launch(t, [factory], {runner: process.env.PI_UI_STAGE_RUNNER});
  await peer.init(['remote_ui', 'composer', 'transcript_render_v1']);
  assert.ok((await peer.command('install').response).result);
  const request = render => peer.request('transcript/render', {source_id: 'actual-entry-7', width: 23, context: peer.context(), render}).response;
  const messageResult = await request({kind: 'message', message: {role: 'custom', customType: 'notice', content: 'source', display: true, details: {actual: 'value'}, timestamp: 1730000000000}, expanded: false, output_pad: 2});
  assert.ok(messageResult.result, JSON.stringify(messageResult));
  assert.match(messageResult.result.lines.join('\n'), /value:2/);
  assert.match(messageResult.result.lines.join('\n'), /\x1b\[/);
  const entryResult = await request({kind: 'entry', entry: {type: 'custom', id: 'actual-entry-7', parentId: 'actual-entry-6', timestamp: '2025-01-01T00:00:00Z', customType: 'saved', data: {actual: 'durable'}}, expanded: true});
  assert.match(entryResult.result.lines.join('\n'), /actual-entry-7:durable/);
  const markdownResult = await request({kind: 'markdown', text: '**source**', message_type: 'assistant', is_streaming: true});
  assert.equal(markdownResult.result.markdown, '**source**:23');
  const toolResult = await request({kind: 'tool', name: 'future', arguments: {}, result: null, expanded: false, is_partial: false, is_error: false, execution_started: false, args_complete: true, show_images: false});
  assert.equal(toolResult.result.render_shell, 'self');
  assert.equal(toolResult.result.lines[0].trimEnd(), 'future');
  await peer.close();
});

test('late terminal renderer registration refuses a missing native consumer', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'octet-pi-transcript-unavailable-'));
  t.after(() => rmSync(directory, {recursive: true, force: true}));
  const factory = join(directory, 'render.ts');
  writeFileSync(factory, `export default pi => pi.registerCommand('install', {handler: () => {
    pi.registerMessageRenderer('notice', () => undefined);
  }});`);
  const peer = launch(t, [factory]);
  await peer.init(['remote_ui', 'composer']);
  const response = await peer.command('install').response;
  assert.equal(response.error?.code, -32601, JSON.stringify(response));
  assert.match(response.error.message, /transcript_render_v1/);
  await peer.close();
});

function harness(t) {
  const store = {id: 8, controller: new AbortController(), state: {alive: true}};
  const runtime = {scope: new AsyncLocalStorage(), loaded: false, stopping: false,
    features: new Set(['remote_ui', 'transcript_render_v1']), tools: new Map(),
    require(feature) { assert.ok(this.features.has(feature), feature); },
    assertOwner(current) { assert.equal(current.state, store.state); assert.ok(current.state.alive, 'live owner'); },
    current() { const current = this.scope.getStore(); this.assertOwner(current); return current; },
  };
  const registry = new TranscriptRenderers(runtime);
  t.after(() => registry.retire(store.state));
  const render = (kind, input, options = {}) => registry.render({source_id: options.source ?? 'native-entry-7', width: options.width ?? 19,
    render: kind === 'tool' ? {kind, ...input} : kind === 'markdown' ? {kind, text: input, message_type: options.messageType ?? 'assistant', is_streaming: options.streaming ?? false}
      : {kind, [kind]: input, expanded: options.expanded ?? false, ...(kind === 'message' ? {output_pad: options.outputPad ?? 1} : {})},
  }, store);
  return {registry, runtime, store, render};
}
const message = {role: 'custom', customType: 'notice', content: 'original', display: true, details: {value: 3}, timestamp: 1730000000000};
const entry = {type: 'custom', id: 'native-entry-7', parentId: 'native-entry-6', timestamp: '2025-01-01T00:00:00Z', customType: 'saved', data: {value: 4}};

test('message renderer receives exact Pi options/details/theme and actual width; approved SGR remains', t => {
  const h = harness(t); let received;
  h.registry.api(0).registerMessageRenderer('notice', (value, options, theme) => {
    received = {value, options}; assert.equal(h.runtime.scope.getStore().factory, 0);
    return new Text(theme.fg('success', `value=${value.details.value} 前😀 long content`), 0, 0);
  });
  const result = h.render('message', message, {width: 13, expanded: true, outputPad: 2});
  assert.deepEqual(received, {value: message, options: {expanded: true, outputPad: 2}});
  assert.equal(result.registered, true); assert.equal(result.markdown, null);
  assert.match(result.lines.join(''), /\x1b\[/); assert.deepEqual(safeLines(result.lines), result.lines);
  assert.ok(result.lines.every(line => visibleWidth(line) <= 13));
});

test('first extension wins, including undefined; repeated registration replaces only its own slot', t => {
  const h = harness(t); let second = 0;
  h.registry.api(1).registerMessageRenderer('notice', () => {second++; return new Text('second', 0, 0);});
  h.registry.api(0).registerMessageRenderer('notice', () => new Text('old', 0, 0));
  h.registry.api(0).registerMessageRenderer('notice', () => undefined);
  assert.deepEqual(h.render('message', message), {registered: true, lines: null, markdown: null});
  assert.equal(second, 0);
  assert.deepEqual(h.render('message', {...message, customType: 'unknown'}), {registered: false, lines: null, markdown: null});
});

test('component reused for resize, rebuilt for expansion/data changes, disposed on retirement', t => {
  const h = harness(t); let callbacks = 0, disposed = 0;
  h.registry.api(0).registerEntryRenderer('saved', () => { callbacks++; return {render: width => [`${width}`], dispose() {disposed++;}}; });
  assert.deepEqual(h.render('entry', entry).lines, ['19']);
  assert.deepEqual(h.render('entry', entry, {width: 37}).lines, ['37']);
  assert.equal(callbacks, 1);
  h.render('entry', entry, {expanded: true}); assert.equal(disposed, 1);
  h.render('entry', {...entry, data: {value: 5}}, {expanded: true}); assert.equal(disposed, 2);
  h.registry.retire(h.store.state); assert.equal(disposed, 3);
});

test('entry callbacks receive real identity/data and message failures fall back while entry failures render', t => {
  const h = harness(t); let observed;
  h.registry.api(0).registerEntryRenderer('saved', (value, options) => {observed = {value, options}; throw new Error('entry failed');});
  const result = h.render('entry', entry, {width: 50, expanded: true});
  assert.deepEqual(observed, {value: entry, options: {expanded: true}});
  assert.match(result.lines.join('\n'), /\[saved\] renderer failed: entry failed/);
  h.registry.api(0).registerMessageRenderer('notice', () => {throw new Error('message failed');});
  assert.deepEqual(h.render('message', message), {registered: true, lines: null, markdown: null});
});

test('Markdown uses latest transformer per extension and chains in load order with exact context', t => {
  const h = harness(t), seen = [];
  h.registry.api(1).registerMarkdownTransformer((text, context) => {seen.push({text, context}); return `${text}:second`;});
  h.registry.api(0).registerMarkdownTransformer(() => 'obsolete');
  h.registry.api(0).registerMarkdownTransformer((text, context) => {seen.push({text, context}); return `${text}:first`;});
  const result = h.render('markdown', '**original**', {width: 31, messageType: 'assistant-thinking', streaming: true});
  assert.deepEqual(result, {registered: true, lines: null, markdown: '**original**:first:second'});
  assert.equal(seen[1].text, '**original**:first'); assert.equal(seen[0].context, seen[1].context);
  assert.deepEqual(seen[0].context, {messageType: 'assistant-thinking', isStreaming: true, availableWidth: 31});
});

test('Markdown preserves current text across exceptions/nonstring returns and accepts empty replacement', t => {
  const h = harness(t);
  h.registry.api(0).registerMarkdownTransformer(() => {throw new Error('ordinary callback failure');});
  h.registry.api(1).registerMarkdownTransformer(() => ({wrong: true}));
  h.registry.api(2).registerMarkdownTransformer(text => `\x1b[31m${text}\x1b[39m`);
  assert.equal(h.render('markdown', 'source').markdown, '\x1b[31msource\x1b[39m');
  h.registry.api(2).registerMarkdownTransformer(() => ''); assert.equal(h.render('markdown', 'source').markdown, '');
});

test('presentation callbacks cannot mutate durable caller input or send unsafe terminal escapes', t => {
  const h = harness(t);
  h.registry.api(0).registerMessageRenderer('notice', value => {value.details.value = 99; return {render: () => ['\x1b]2;unsafe\x07']};});
  assert.throws(() => h.render('message', message), /unsafe escape/); assert.equal(message.details.value, 3);
  h.registry.api(0).registerMarkdownTransformer(() => '\x1b[2Jerase');
  assert.throws(() => h.render('markdown', 'source'), /unsafe escape/);
});

test('cancellation and stale ownership cannot publish component or transformed Markdown', t => {
  const h = harness(t); let disposed = 0;
  h.registry.api(0).registerMessageRenderer('notice', () => {
    h.store.controller.abort(new Error('cancelled'));
    return {render: () => ['late'], dispose() {disposed++;}};
  });
  assert.throws(() => h.render('message', message), /cancelled/); assert.equal(disposed, 1);
  const other = harness(t);
  other.registry.api(0).registerMarkdownTransformer(text => {other.store.state.alive = false; return `${text}late`;});
  assert.throws(() => other.render('markdown', 'source'), /live owner/);
});

test('late registration requires a current owner and bounded caches dispose evicted components', t => {
  const h = harness(t); let disposed = 0;
  h.registry.api(0).registerEntryRenderer('saved', () => ({render: () => ['entry'], dispose() {disposed++;}}));
  for (let i = 0; i < 129; i++) h.render('entry', entry, {source: `native-${i}`});
  assert.equal(disposed, 1); h.registry.retire(h.store.state); assert.equal(disposed, 129);
  h.runtime.loaded = true;
  assert.throws(() => h.registry.api(0).registerMarkdownTransformer(text => text));
  h.runtime.scope.run(h.store, () => h.registry.api(0).registerMarkdownTransformer(text => text));
  h.runtime.stopping = true;
  assert.throws(() => h.registry.api(0).registerMessageRenderer('notice', () => undefined), /retired/);
});

test('unknown profiles, malformed options and frame overflow are rejected', t => {
  const h = harness(t);
  assert.throws(() => h.render('markdown', 'source', {messageType: 'tool'}), /Markdown message type/);
  assert.throws(() => h.render('message', message, {outputPad: -1}), /outputPad/);
  assert.throws(() => h.render('message', message, {width: 0}), /geometry/);
  h.registry.api(0).registerMessageRenderer('notice', () => ({render: () => Array(257).fill('')}));
  assert.throws(() => h.render('message', message), /bounds_exceeded/);
});

test('tool resolver chain sees future tools, preserves next and can suppress a base renderer', t => {
  const h = harness(t), order = [];
  h.registry.api(1).registerToolRenderer((name, next) => { order.push(`second:${name}`); return next() ?? {renderShell: 'self', renderCall: () => new Text('future', 0, 0)}; });
  h.registry.api(0).registerToolRenderer((name, next) => { order.push(`first:${name}`); return next(); });
  const tool = {name: 'future', arguments: {}, result: null, expanded: false, is_partial: false, is_error: false, execution_started: false, args_complete: true, show_images: false};
  assert.deepEqual(h.render('tool', tool), {registered: true, lines: ['future             '], markdown: null, render_shell: 'self'});
  assert.deepEqual(order, ['first:future', 'second:future']);
  h.runtime.tools.set('future', {factory: 2, definition: {renderCall: () => new Text('base', 0, 0)}});
  assert.deepEqual(h.render('tool', tool).lines.map(line => line.trimEnd()), ['base']);
  const other = harness(t); other.runtime.tools.set('future', h.runtime.tools.get('future'));
  other.registry.api(0).registerToolRenderer(() => undefined);
  assert.equal(other.render('tool', tool).registered, false);
});

test('tool call/result contexts retain state and lastComponent, preserve exact result, and never render an absent result', t => {
  const h = harness(t), seen = []; let disposed = 0;
  h.store.state.workspace = '/real/workspace';
  h.runtime.tools.set('native', {factory: 0, definition: { renderShell: 'self',
    renderCall(args, theme, context) {
      seen.push({args, context}); context.state.count = (context.state.count ?? 0) + 1;
      return context.lastComponent ?? {render: width => [theme.fg('success', `width=${width}`)], dispose() {disposed++;}};
    },
    renderResult(result, options, _theme, context) { seen.push({result, options, context}); return new Text('result', 0, 0); },
  }});
  const input = {name: 'native', arguments: {value: 2}, result: null, expanded: true, is_partial: false, is_error: false, execution_started: true, args_complete: true, show_images: false};
  const call = h.render('tool', input, {width: 29}); assert.equal(seen.length, 1);
  assert.ok(call.lines[0].includes('width=29')); assert.equal(seen[0].context.lastComponent, undefined);
  const result = {content: [{type: 'text', text: 'actual output'}], details: {value: 3}, isError: true};
  h.render('tool', {...input, result, is_error: true, is_partial: true}, {width: 17});
  assert.equal(seen[1].context.state, seen[0].context.state); assert.equal(seen[1].context.state.count, 2);
  assert.ok(seen[1].context.lastComponent); assert.equal(seen[2].context.toolCallId, 'native-entry-7');
  assert.equal(seen[2].context.cwd, '/real/workspace'); assert.equal(seen[2].context.isError, true);
  assert.deepEqual(seen[2].options, {expanded: true, isPartial: true}); assert.deepEqual(seen[2].result, result);
  h.registry.retire(h.store.state); assert.equal(disposed, 1);
});

test('Markdown tabs are preserved, while unsafe OSC and malformed tool booleans still fail', t => {
  const h = harness(t); h.registry.api(0).registerMarkdownTransformer(text => text);
  assert.equal(h.render('markdown', '\tcode\n\tmore').markdown, '\tcode\n\tmore');
  h.runtime.tools.set('native', {factory: 0, definition: {renderCall: () => new Text('call', 0, 0)}});
  assert.throws(() => h.render('tool', {name: 'native', arguments: {}, result: null}), /executionStarted/);
});

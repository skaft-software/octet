import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { createHostedRunner, ExtensionRunner, hostedToolCatalog } from '../lib/extension-runner.mjs';
import { createBuiltinDefinition } from '../lib/builtin-tools.mjs';
import { withToolHost } from '../lib/tool-host.mjs';

function host() {
  const calls = [], events = [], state = { alive: true, workspace: '/workspace' };
  const store = { id: 4, method: 'tool/call', live: true, state, controller: new AbortController() };
  let effects = 0;
  const definition = { name: 'probe', label: 'Probe', description: 'Probe tool', parameters: { type: 'object' },
    execute: async () => { effects++; return { content: [{ type: 'text', text: 'raw effect' }] }; } };
  const runtime = { tools: new Map([['probe', { definition, factory: '/extensions/probe.ts' }]]),
    scope: new AsyncLocalStorage(), entrypoint: factory => factory,
    features: new Set(['tool_composition_v1', 'composition_builtin_tools_v1', 'composition_preflight_v1']),
    require(feature) { assert.ok(this.features.has(feature), feature); },
    assertOwner(value) { assert.equal(value.state, state); assert.ok(state.alive); },
    async runEvent(type, event, bound) { assert.equal(bound, store); events.push({ type, event }); },
    async hostCall(method, params, bound) {
      assert.equal(bound, store); calls.push({ method, params });
      const tool_call = { id: 'outer/1', name: params.name, arguments: { ...params.arguments, prepared: true } };
      return { value: method === 'composition/prepare' ? { prepared_id: 'receipt', tool_call } : {
        tool_call: { ...tool_call, arguments: params.arguments }, content: [{ Text: 'canonical native result' }], is_error: false,
      } };
    },
  };
  return { runtime, store, definition, calls, events, effects: () => effects,
    run: fn => runtime.scope.run(store, () => withToolHost(runtime, store, fn)) };
}

test('hosted runner is the intercepted live catalog with original definition and source identities', t => {
  const h = host(), runner = createHostedRunner(h.runtime), anchor = Symbol('anchor');
  h.definition[anchor] = {};
  assert.throws(() => new ExtensionRunner(), /unsupported_feature/);
  const original = ExtensionRunner.prototype.getAllRegisteredTools;
  let captured;
  ExtensionRunner.prototype.getAllRegisteredTools = function () {
    captured = { runner: this, tools: original.call(this) }; return captured.tools;
  };
  t.after(() => { ExtensionRunner.prototype.getAllRegisteredTools = original; });
  const catalog = hostedToolCatalog(runner);
  assert.equal(captured.runner, runner);
  assert.equal(catalog.get('probe').definition, h.definition);
  assert.equal(captured.tools[0].definition[anchor], h.definition[anchor]);
  assert.equal(captured.tools[0].sourceInfo.path, '/extensions/probe.ts');
  assert.throws(() => captured.tools[0].definition.execute('guest', {}), /live model-tool/);
  assert.equal(h.effects(), 0);
});

test('captured execution consumes real preparation once, preserving native settlement and lifecycle', async () => {
  const h = host(), runner = createHostedRunner(h.runtime), captured = runner.getAllRegisteredTools()[0];
  await h.run(async () => {
    const input = { original: true }, event = { type: 'tool_call', toolCallId: 'guest', toolName: 'probe', input };
    await runner.emit({ type: 'tool_execution_start', toolCallId: 'guest', toolName: 'probe', args: input });
    assert.equal(await runner.emitToolCall(event), undefined);
    assert.deepEqual(input, { original: true, prepared: true });
    assert.equal(h.calls.length, 1);
    assert.equal(h.calls[0].method, 'composition/prepare');
    const result = await captured.definition.execute('guest', input);
    assert.equal(result.content[0].text, 'canonical native result');
    assert.equal(h.calls[1].params.prepared_id, 'receipt');
    assert.equal(h.calls[1].params.builtin, undefined);
    const patch = await runner.emitToolResult({ type: 'tool_result', toolCallId: 'guest', toolName: 'probe', content: [{ type: 'text', text: 'fabricated' }] });
    assert.deepEqual(patch, result);
    await runner.emit({ type: 'tool_execution_end', toolCallId: 'guest', toolName: 'probe', result: patch });
    await assert.rejects(captured.definition.execute('guest', input), /receipt/);
    await assert.rejects(runner.emitToolResult({ type: 'tool_result', toolCallId: 'guest', toolName: 'probe' }), /completed native/);
  });
  assert.equal(h.effects(), 0);
  assert.equal(h.calls.length, 2);
  assert.deepEqual(h.events.map(event => event.type), ['tool_execution_start', 'tool_execution_end']);
});

test('preparation rejects changed arguments and stale parents before dispatch', async () => {
  const h = host(), runner = createHostedRunner(h.runtime), captured = runner.getAllRegisteredTools()[0];
  await h.run(async () => {
    const input = {};
    await runner.emitToolCall({ type: 'tool_call', toolCallId: 'guest', toolName: 'probe', input });
    await assert.rejects(captured.definition.execute('guest', { changed: true }), /receipt/);
    await assert.rejects(runner.emitToolCall({ type: 'tool_call', toolCallId: 'guest', toolName: 'probe', input }), /already prepared/);
    h.store.live = false;
    assert.throws(() => captured.definition.execute('guest', input), /live model-tool/);
  });
  assert.equal(h.calls.length, 1);
});

test('native veto is effect free and its transformed result is authoritative', async () => {
  const h = host(), runner = createHostedRunner(h.runtime);
  h.runtime.hostCall = async (method, params) => {
    assert.equal(method, 'composition/prepare');
    return { value: { block: true, reason: 'native hook veto', outcome: {
      tool_call: { id: 'outer/1', name: params.name, arguments: params.arguments }, content: [{ Text: 'native transformed veto' }], is_error: true,
    } } };
  };
  await h.run(async () => {
    const event = { type: 'tool_call', toolCallId: 'guest', toolName: 'probe', input: {} };
    assert.deepEqual(await runner.emitToolCall(event), { block: true, reason: 'native hook veto' });
    await assert.rejects(runner.getAllRegisteredTools()[0].definition.execute('guest', {}), /receipt/);
    const result = await runner.emitToolResult({ ...event, type: 'tool_result', content: [] });
    assert.equal(result.content[0].text, 'native transformed veto');
    assert.equal(result.isError, true);
  });
  assert.equal(h.effects(), 0);
});

test('builtin preparation translates Pi arguments before native schema admission without translating twice', async () => {
  const h = host(), runner = createHostedRunner(h.runtime), bash = createBuiltinDefinition('bash', '/workspace');
  await h.run(async () => {
    const input = { command: 'printf ok', timeout: 1.25 };
    await runner.emitToolCall({ type: 'tool_call', toolCallId: 'shell', toolName: 'bash', input });
    assert.deepEqual(h.calls[0].params, { name: 'bash', builtin: true, arguments: { command: 'printf ok', cwd: '/workspace', timeout_ms: 1250 } });
    await bash.execute('shell', input);
    assert.deepEqual(h.calls[1].params.arguments, { command: 'printf ok', cwd: '/workspace', timeout_ms: 1250, prepared: true });
    assert.equal(h.calls[1].params.builtin, true);
    assert.equal(h.calls[1].params.prepared_id, 'receipt');
  });
});

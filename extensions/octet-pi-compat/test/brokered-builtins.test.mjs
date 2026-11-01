import assert from 'node:assert/strict';
import test from 'node:test';
import { createBuiltinDefinition } from '../lib/builtin-tools.mjs';
import { withToolHost } from '../lib/tool-host.mjs';

function host() {
  const calls = [], state = { alive: true, workspace: '/workspace' };
  const store = { id: 3, method: 'tool/call', live: true, state, controller: new AbortController() };
  const runtime = { features: new Set(['tool_composition_v1', 'composition_builtin_tools_v1']),
    require(feature) { assert.ok(this.features.has(feature), feature); },
    assertOwner(value) { assert.equal(value.state, state); assert.ok(state.alive); },
    hostCall: async (method, params) => { calls.push({ method, params }); return { value: {
      tool_call: { id: 'outer/1', name: params.name, arguments: params.arguments },
      content: [{ Text: 'native output' }], is_error: false,
    } }; },
  };
  return { runtime, store, calls, run: fn => withToolHost(runtime, store, fn) };
}

test('builtin factories construct faithful Pi definitions without an agent, install or effects', () => {
  for (const name of ['read', 'write', 'edit', 'bash', 'powershell', 'grep', 'find', 'ls']) {
    const definition = createBuiltinDefinition(name, '/workspace');
    assert.equal(definition.name, name);
    assert.equal(typeof definition.execute, 'function');
    assert.equal(definition.parameters.type, 'object');
  }
  assert.throws(() => createBuiltinDefinition('read', '/workspace', { operations: {} }), /unsupported_feature/);
});

test('direct builtin execution has no raw Pi fallback and cannot start outside a live tool', async () => {
  const read = createBuiltinDefinition('read', '/workspace');
  await assert.rejects(read.execute('id', { path: 'a' }), /live model-tool/);
  const h = host();
  for (const method of ['command/execute', 'hook/run']) {
    h.store.method = method;
    await h.run(() => assert.rejects(read.execute('id', { path: 'a' }), /live model-tool/));
  }
  assert.equal(h.calls.length, 0);
});

test('builtin read uses displaced native dispatch with actual cwd, not replacement-by-name recursion', async () => {
  const h = host();
  const read = createBuiltinDefinition('read', '/workspace/subdir');
  const result = await h.run(() => read.execute('id', { path: 'a', offset: 2, limit: 3 }));
  assert.equal(result.content[0].text, 'native output');
  assert.deepEqual(h.calls, [{ method: 'composition/call', params: {
    name: 'read', arguments: { path: '/workspace/subdir/a', offset: 2, limit: 3 }, full_outcome: true, builtin: true,
  } }]);
});

test('builtin shell seconds become brokered milliseconds and cancellation never starts an effect', async () => {
  const h = host();
  const bash = createBuiltinDefinition('bash', '/workspace');
  await h.run(() => bash.execute('id', { command: 'printf ok', timeout: 1.25 }));
  assert.deepEqual(h.calls[0].params.arguments, { command: 'printf ok', cwd: '/workspace', timeout_ms: 1250 });
  const abort = new AbortController(); abort.abort();
  await h.run(() => assert.rejects(bash.execute('id', { command: 'bad' }, abort.signal), /abort/i));
  h.store.state.alive = false;
  await h.run(() => assert.rejects(bash.execute('id', { command: 'bad' })));
  assert.equal(h.calls.length, 1);
});

test('missing native builtin contract, unknown options and invalid timeouts refuse before dispatch', async () => {
  const h = host(), read = createBuiltinDefinition('read');
  h.runtime.features.delete('composition_builtin_tools_v1');
  await h.run(() => assert.rejects(read.execute('id', { path: 'a' }), /composition_builtin_tools_v1/));
  const bash = createBuiltinDefinition('bash');
  for (const timeout of [0, -1, Infinity, NaN, 2147484]) {
    await h.run(() => assert.rejects(bash.execute('id', { command: 'bad', timeout }), /timeout/));
  }
  assert.equal(h.calls.length, 0);
});

test('native private read text projects to Pi text without numbered headers and keeps continuation bounds', async () => {
  const h = host(), read = createBuiltinDefinition('read');
  h.runtime.hostCall = async (_method, params) => ({ value: {
    tool_call: { id: 'outer/1', name: params.name, arguments: params.arguments },
    content: [{ Text: 'native path/hash header\n2: alpha\n3: beta' }], is_error: false,
    builtin_output: { value: { path: '/workspace/a', content: 'alpha\nbeta', start_line: 2, end_line: 3, total_lines: 4, next_offset: 4 }, duration_ms: 3 },
  } });
  const result = await h.run(() => read.execute('read', { path: 'a', offset: 2, limit: 2 }));
  assert.equal(result.content[0].text, 'alpha\nbeta\n\n[Showing lines 2-3 of 4. Use offset=4 to continue.]');
  assert.equal(result.structuredContent, undefined);
});

test('native shell private output projects actual Pi output schema and native measured duration', async () => {
  const h = host(), bash = createBuiltinDefinition('bash');
  h.runtime.hostCall = async (_method, params) => ({ value: {
    tool_call: { id: 'outer/1', name: params.name, arguments: params.arguments }, content: [{ Text: 'native shell presentation' }], is_error: false,
    builtin_output: { value: { stdout: 'out', stderr: 'err', exit_code: 0, truncated_stdout: false, truncated_stderr: false }, duration_ms: 1250 },
  } });
  const result = await h.run(() => bash.execute('shell', { command: 'printf out; printf err >&2' }));
  assert.equal(result.content[0].text, 'outerr');
  assert.deepEqual(result.structuredContent, { output: 'outerr', truncated: false, exit_code: 0, wall_time_seconds: 1.25 });
});

test('native result hooks retain transformed presentation instead of reconstructing original builtin text', async () => {
  const h = host(), read = createBuiltinDefinition('read');
  const result = await h.run(() => read.execute('read', { path: 'a' }));
  assert.equal(result.content[0].text, 'native output');
});

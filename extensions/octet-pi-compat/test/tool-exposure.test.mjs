import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { validateTool, toolWire, registerTool, getAllTools, setActiveTools, createToolContext, prepareToolLoadout, executeRegisteredTool } from '../lib/tools.mjs';

const exposures = ['direct', 'codemode', 'deferred', 'model-only', 'hidden'];
const definition = exposure => ({ name: exposure.replace('-', '_'), description: 'Exposure probe', exposure, parameters: { type: 'object' }, execute: async () => ({ content: [] }) });
function runtime() {
  const store = { id: 1, state: { owner: { session_id: 's', extension_instance_id: 'i', process_generation: 1 } }, live: true, controller: new AbortController() };
  return { tools: new Map(), current: () => store, store, scope: new AsyncLocalStorage(), require() {}, features: new Set(['tool_composition_v1']), backgroundError(error) { throw error; } };
}

test('Pi 1.0.2 exposure modes are preserved on the host wire and unknown modes refuse', () => {
  for (const exposure of exposures) {
    const tool = definition(exposure);
    validateTool(tool);
    assert.equal(toolWire(tool.name, { definition: tool }).exposure, exposure);
  }
  assert.throws(() => validateTool(definition('unknown')), /tool.exposure/);
});

test('Pi exposure controls activation and callable loadout without inventing host catalog entries', () => {
  const rt = runtime();
  for (const exposure of exposures) registerTool(rt, '/probe.ts', definition(exposure));
  const all_tools = exposures.map(exposure => ({ name: definition(exposure).name, description: 'Native description', parameters: { type: 'object' }, exposure }));
  const calls = [];
  rt.transport = { requestSync(method, params) {
    calls.push([method, params]);
    if (method === 'tools/snapshot') return { active_tools: ['direct', 'model_only'], all_tools };
    return {};
  } };
  assert.deepEqual(getAllTools(rt, '/probe.ts').map(tool => tool.exposure), exposures);
  setActiveTools(rt, '/probe.ts', ['hidden', 'direct', 'codemode', 'model_only', 'unknown']);
  assert.deepEqual(calls.at(-1)[1].names, ['direct', 'codemode', 'model_only']);
  rt.tools.get('direct').definition.prepareLoadout = loadout => {
    assert.deepEqual(loadout.callable.map(tool => tool.name), ['direct', 'codemode', 'deferred']);
    assert.equal(loadout.getExposure('model_only'), 'model-only');
  };
  const declared = all_tools.filter(tool => ['direct', 'model_only'].includes(tool.name));
  assert.deepEqual(prepareToolLoadout(rt, rt.store, declared), declared);
});

test('nested Pi calls reject hidden/model-only tools before host effects while retaining native admission', async () => {
  const rt = runtime();
  for (const exposure of exposures) registerTool(rt, '/probe.ts', definition(exposure));
  const calls = [];
  rt.transport = {
    requestSync() { return { tools: exposures.map(exposure => ({ name: definition(exposure).name, description: 'Host tool', parameters: { type: 'object' }, exposure })) }; },
    async request(method, params) { calls.push([method, params]); throw Error('native admission refused'); },
  };
  const context = await createToolContext(rt, { name: 'direct' }, rt.store, {});
  assert.deepEqual(context.tools.map(tool => tool.name), ['direct', 'codemode', 'deferred']);
  for (const name of ['hidden', 'model_only']) await assert.rejects(context.executeTool(name, {}), /not callable/);
  assert.equal(calls.length, 0);
  await assert.rejects(context.executeTool('codemode', {}), /native admission refused/);
  assert.equal(calls.length, 1);
});


test('native dispatch can supply its admitted execution binding without changing callback scope', async () => {
  const rt = runtime(), tool = definition('direct');
  let originalCalls = 0;
  tool.execute = async () => { originalCalls++; return { content: [] }; };
  registerTool(rt, '/probe.ts', tool);
  rt.flush = async store => assert.equal(store, rt.store);
  const selected = { ...tool, execute: async (id, args, signal, update, context) => {
    assert.equal(id, '1'); assert.deepEqual(args, { value: 2 });
    assert.equal(signal, rt.store.controller.signal); assert.equal(update, undefined);
    assert.equal(rt.scope.getStore(), rt.store); assert.deepEqual(context, { admitted: true });
    return { content: [{ type: 'text', text: 'bound' }] };
  } };
  const result = await executeRegisteredTool(rt, { name: 'direct', arguments: { value: 2 } }, rt.store, { admitted: true }, selected);
  assert.deepEqual(result, { content: [{ type: 'text', text: 'bound' }], is_error: false });
  assert.equal(originalCalls, 0);
  await executeRegisteredTool(rt, { name: 'direct', arguments: {} }, rt.store, {});
  assert.equal(originalCalls, 1);
});

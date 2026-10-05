import test from 'node:test';
import assert from 'node:assert/strict';
import { setModel, setThinkingLevel } from '../lib/model-control.mjs';
import { host, owner, launch } from './helper.mjs';
import { mkdtempSync, writeFileSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

function fixture() {
  const controller = new AbortController();
  const store = { id: 17, live: true, controller, state: { owner, host: structuredClone(host) } };
  const calls = [];
  const runtime = {
    stopping: false, active: new Map([[17, { controller }]]),
    require: feature => assert.equal(feature, 'session_control_v1'),
    assertOwner: value => assert.equal(value, store),
    track: promise => promise,
    hostCall: (method, params) => { calls.push({ method, params }); return runtime.reply; },
    transport: { requestSync: (method, params) => { calls.push({ method, params }); return runtime.receipt; } },
  };
  return { runtime, store, calls };
}
const receipt = { selected: true, model_view: { ...host.model_view, id: 'chosen-model' }, reasoning: 'high' };

test('async setModel does not publish before an authoritative receipt', async () => {
  const { runtime, store, calls } = fixture();
  let settle; runtime.reply = new Promise(resolve => { settle = resolve; });
  const selected = setModel(runtime, store, { provider: 'test', id: 'chosen-model' });
  assert.equal(store.state.host.model_view.id, 'test-model');
  assert.equal(calls[0].method, 'model/select');
  assert.deepEqual(calls[0].params.selection, { operation: 'model', provider: 'test', id: 'chosen-model' });
  settle(receipt); assert.equal(await selected, true);
  assert.equal(store.state.host.model_view.id, 'chosen-model');
  assert.equal(store.state.host.reasoning, 'high');
});

test('unknown models and host failures cannot fabricate selection', async () => {
  const { runtime, store } = fixture();
  runtime.reply = Promise.resolve({ selected: false });
  assert.equal(await setModel(runtime, store, { provider: 'test', id: 'unknown' }), false);
  assert.equal(store.state.host.model_view.id, 'test-model');
  runtime.reply = Promise.reject(new Error('durable append failed'));
  await assert.rejects(setModel(runtime, store, { provider: 'test', id: 'known' }), /durable append failed/);
  assert.equal(store.state.host.model_view.id, 'test-model');
});

test('synchronous thinking getter follows the host-clamped selection', () => {
  const { runtime, store, calls } = fixture();
  runtime.receipt = { ...receipt, reasoning: 'low' };
  assert.equal(setThinkingLevel(runtime, store, 'high'), undefined);
  assert.equal(store.state.host.reasoning, 'low');
  assert.equal(calls[0].params.parent_request_id, 17);
  assert.deepEqual(calls[0].params.resource_owner, owner);
  assert.deepEqual(calls[0].params.selection, { operation: 'thinking', level: 'high' });
  assert.throws(() => setThinkingLevel(runtime, store, 'ultra'));
});

test('thinking transport errors do not publish an optimistic level', () => {
  const { runtime, store } = fixture();
  runtime.transport.requestSync = () => { throw new Error('active-turn refusal'); };
  assert.throws(() => setThinkingLevel(runtime, store, 'high'), /active-turn refusal/);
  assert.equal(store.state.host.reasoning, null);
});

test('real adapter command completes async model and synchronous thinking receipts', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'octet-model-controls-'));
  const entry = join(directory, 'probe.ts'), trace = join(directory, 'trace.json');
  writeFileSync(entry, `import {writeFileSync} from 'node:fs'; export default pi => {
    pi.registerCommand('select', {handler:async (_,ctx) => {
      const selected=await pi.setModel({...ctx.model,id:'chosen-model'});
      pi.setThinkingLevel('high');
      writeFileSync(${JSON.stringify(trace)},JSON.stringify({selected,id:ctx.model.id,level:pi.getThinkingLevel()}));
    }});
  };`);
  const adapter = launch(t, [entry], {auto:false});
  await adapter.init(['session_control_v1']); await adapter.start({reasoning:'off'});
  const command = adapter.command('select', [], {reasoning:'off'});
  const model = await adapter.wait(frame => frame.method === 'model/select');
  assert.equal(model.params.parent_request_id, command.id);
  assert.deepEqual(model.params.resource_owner, owner);
  adapter.send({jsonrpc:'2.0',id:model.id,result:receipt});
  const thinking = await adapter.wait(frame => frame.method === 'model/select' && frame.params.selection.operation === 'thinking');
  adapter.send({jsonrpc:'2.0',id:thinking.id,result:{...receipt,reasoning:'low'}});
  const response = await command.response;
  assert.ok(response.result, JSON.stringify(response));
  assert.deepEqual(JSON.parse(readFileSync(trace,'utf8')), {selected:true,id:'chosen-model',level:'low'});
  await adapter.close();
});

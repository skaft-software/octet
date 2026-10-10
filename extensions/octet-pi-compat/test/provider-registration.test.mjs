import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { existsSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';
import { modelRegistry, registerProvider, unregisterProvider, startProviderRegistration, validateProviderInitialization, prepareProviderStream, startProviderStream, cancelProviderStream } from '../lib/providers.mjs';

const definition = streamSimple => ({ baseUrl: 'http://127.0.0.1:9/local/', apiKey: 'explicit-local-dummy', api: 'openai-completions',
  models: [{ id: 'virtual/model', name: 'Virtual Local', reasoning: false, input: ['text', 'image'], cost: { input: 1, output: 2, cacheRead: 0.1, cacheWrite: 0.2 }, contextWindow: 8192, maxTokens: 1024 }], streamSimple });
function fixture() {
  const frames = [], pending = [], store = { controller: new AbortController() };
  const runtime = { initialized: false, stopping: false, features: new Set(['provider_proxy_v1']),
    require: feature => assert.ok(runtime.features.has(feature)), current: () => store,
    track: promise => { pending.push(promise); return promise; }, scope: { run: (_, fn) => fn() },
    backgroundError: error => { throw error; },
    transport: { request: async (method, params) => { frames.push({ method, params }); return { revision: 1 }; }, notify: async (method, params) => { frames.push({ method, params }); } },
  };
  return { runtime, frames, pending, store };
}
// Fixture hang guard for cross-process effects (a child file write, a reverse
// registration). The window matches this harness's own 7 s round-trip timeout:
// half a second is below one loaded Node round trip, which made the guard fire
// on work that was still in progress.
async function until(predicate) {
  for (let i = 0; i < 1400; i++) { if (predicate()) return; await new Promise(resolve => setTimeout(resolve, 5)); }
  assert.fail('bounded fixture timeout');
}
async function registered(stream) {
  const value = fixture(); registerProvider(value.runtime, 0, 'pi-local-provider', definition(stream));
  assert.equal(value.frames.length, 0, 'no reverse registration before initialize reply');
  validateProviderInitialization(value.runtime); value.runtime.initialized = true; startProviderRegistration(value.runtime);
  await until(() => value.frames.some(frame => frame.method === 'providers/complete'));
  return value;
}
const request = () => ({ system: 'native system', messages: [{ User: { content: [{ Text: 'native input' }] } }], tools: [], tool_choice: 'auto', stop: [], output_format: { type: 'text' }, reasoning: { type: 'off' }, max_output_tokens: 32, cache_retention: 'short', session_id: null });
function params(value, id = 'stream-1') {
  return { stream_id: id, provider_id: 'pi-local-provider', model_id: value.frames[0].params.models[0].id, request: request(), authorization_lease: null };
}

test('startup batch uses native registration, preserves exact metadata and keeps explicit keys process-local', async () => {
  const value = await registered(() => {}), registration = value.frames[0];
  assert.equal(registration.method, 'providers/register');
  assert.equal(registration.params.provider.auth.kind, 'none');
  assert.equal(registration.params.models[0].api_name, 'virtual/model');
  assert.match(registration.params.models[0].id, /^m-[0-9a-f]{32}$/);
  assert.equal(registration.params.models[0].pi_metadata.pricing.input, 1_000_000);
  assert.deepEqual(registration.params.models[0].pi_metadata.input, ['text', 'image']);
  assert.ok(!JSON.stringify(registration).includes('explicit-local-dummy'));
});

test('replacement uses native update, unregister uses native owner-fenced removal', async () => {
  const value = await registered(() => {});
  registerProvider(value.runtime, 0, 'pi-local-provider', definition(() => {})); await Promise.all(value.pending);
  assert.equal(value.frames.at(-1).method, 'providers/update');
  unregisterProvider(value.runtime, 0, 'pi-local-provider'); await Promise.all(value.pending);
  assert.equal(value.frames.at(-1).method, 'providers/unregister');
  await assert.rejects(prepareProviderStream(value.runtime, params(value), value.store), /route unavailable/);
});

test('custom stream receives logical selection, canonical context and only the explicitly supplied key', async () => {
  let observed;
  const value = await registered((model, context, options) => (async function* () {
    observed = { model, context, key: options.apiKey };
    const partial = { content: [{ type: 'text', text: '' }] };
    yield { type: 'start', partial };
    yield { type: 'text_start', contentIndex: 0, partial };
    partial.content[0].text = 'local'; yield { type: 'text_delta', contentIndex: 0, delta: 'local', partial };
    yield { type: 'text_end', contentIndex: 0, content: 'local', partial };
    yield { type: 'done', reason: 'stop', message: { content: partial.content, usage: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0, totalTokens: 3 } } };
  })());
  assert.deepEqual(await prepareProviderStream(value.runtime, params(value), value.store), { stream_id: 'stream-1', accepted: true });
  assert.equal(observed, undefined, 'callback starts only after host stream acceptance reply');
  startProviderStream(value.runtime, 'stream-1');
  await until(() => value.frames.some(frame => frame.params.kind === 'finished'));
  assert.equal(observed.model.id, 'virtual/model'); assert.equal(observed.key, 'explicit-local-dummy');
  assert.equal(observed.context.messages[0].content[0].text, 'native input');
  const events = value.frames.filter(frame => frame.method === 'provider/event');
  assert.deepEqual(events.map(frame => frame.params.kind), ['started', 'text_start', 'text_delta', 'text_end', 'usage', 'finished']);
  assert.deepEqual(events.map(frame => frame.params.sequence), [0, 1, 2, 3, 4, 5]);
});

test('oversized native payload becomes a contiguous terminal error, not an invalid wire frame', async () => {
  const value = await registered(() => (async function* () {
    const partial = {content:[{type:'text',text:''}]};
    yield {type:'start',partial};
    yield {type:'text_start',contentIndex:0,partial};
    yield {type:'text_delta',contentIndex:0,delta:'x'.repeat(70000),partial};
  })());
  await prepareProviderStream(value.runtime, params(value), value.store);
  startProviderStream(value.runtime, 'stream-1');
  await until(() => value.frames.some(frame => frame.params.kind === 'error'));
  const events = value.frames.filter(frame => frame.method === 'provider/event');
  assert.deepEqual(events.map(frame => [frame.params.sequence,frame.params.kind]), [[0,'started'],[1,'text_start'],[2,'error']]);
});

test('native cancellation aborts the callback signal without waiting for an uncooperative iterator', async () => {
  let signal;
  const value = await registered((_, __, options) => { signal = options.signal; return { [Symbol.asyncIterator]() { return this; }, next: () => new Promise(() => {}), return: () => new Promise(() => {}) }; });
  await prepareProviderStream(value.runtime, params(value), value.store); startProviderStream(value.runtime, 'stream-1');
  assert.equal(signal.aborted, false); cancelProviderStream(value.runtime, 'stream-1'); assert.equal(signal.aborted, true);
});

for (const operation of ['replace', 'remove']) test(`acknowledged native ${operation} aborts an already accepted custom callback`, async () => {
  let signal;
  const value = await registered((_, __, options) => { signal = options.signal; return { [Symbol.asyncIterator]() { return this; }, next: () => new Promise(() => {}) }; });
  await prepareProviderStream(value.runtime, params(value), value.store);
  startProviderStream(value.runtime, 'stream-1');
  assert.equal(signal.aborted, false);
  if (operation === 'replace') registerProvider(value.runtime, 0, 'pi-local-provider', definition(() => {}));
  else unregisterProvider(value.runtime, 0, 'pi-local-provider');
  await Promise.all(value.pending);
  assert.equal(signal.aborted, true);
});

test('real adapter transport registers after initialize and emits the native accepted/event envelopes', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'pi-provider-wire-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const entry = join(directory, 'provider.mjs');
  const provider = definition(undefined);
  const cancelledPath = join(directory, 'cancelled.txt');
  await writeFile(entry, `import { writeFileSync } from 'node:fs';
  export default pi => pi.registerProvider('pi-local-provider', {
    ${JSON.stringify(provider).slice(1, -1)},
    streamSimple: (_, context, options) => (async function* () {
      if (options.apiKey !== 'explicit-local-dummy') throw new Error('local key missing');
      const partial = { content: [{type:'text',text:''}] };
      yield {type:'start',partial};
      if (context.systemPrompt === 'cancel-native') {
        await new Promise(resolve => options.signal.addEventListener('abort', resolve, {once:true}));
        writeFileSync(${JSON.stringify(cancelledPath)}, String(options.signal.aborted));
        return;
      }
      yield {type:'text_start',contentIndex:0,partial};
      partial.content[0].text = 'local';
      yield {type:'text_delta',contentIndex:0,delta:'local',partial};
      yield {type:'text_end',contentIndex:0,content:'local',partial};
      yield {type:'done',reason:'stop',message:{content:partial.content,usage:{input:1,output:2,cacheRead:0,cacheWrite:0,totalTokens:3}}};
    })()
  });`);
  const peer = launch(t, [entry]);
  await peer.init(['provider_proxy_v1']);
  const registration = await peer.wait(frame => frame.method === 'providers/register');
  assert.ok(!JSON.stringify(registration).includes('explicit-local-dummy'));
  peer.send({jsonrpc:'2.0',id:registration.id,result:{revision:1,provider_ids:['pi-local-provider'],model_ids:[registration.params.models[0].id]}});
  await peer.wait(frame => frame.method === 'providers/complete');
  const streamId = 'native-stream-1';
  const accepted = await peer.request('provider/stream', {stream_id:streamId,provider_id:'pi-local-provider',model_id:registration.params.models[0].id,request:request()}).response;
  assert.deepEqual(accepted.result, {stream_id:streamId,accepted:true});
  for (const [sequence, kind] of ['started','text_start','text_delta','text_end','usage','finished'].entries()) {
    const event = await peer.wait(frame => frame.method === 'provider/event' && frame.params.sequence === sequence);
    assert.equal(event.params.stream_id, streamId);
    assert.equal(event.params.kind, kind);
  }
  const cancellationId = 'native-stream-cancel';
  const cancellation = await peer.request('provider/stream', {stream_id:cancellationId,provider_id:'pi-local-provider',model_id:registration.params.models[0].id,request:{...request(),system:'cancel-native'}}).response;
  assert.deepEqual(cancellation.result, {stream_id:cancellationId,accepted:true});
  await peer.wait(frame => frame.method === 'provider/event' && frame.params.stream_id === cancellationId && frame.params.kind === 'started');
  peer.notify('provider/cancel', {stream_id:cancellationId,reason:'local test cancellation'});
  await until(() => existsSync(cancelledPath));
  assert.equal(readFileSync(cancelledPath, 'utf8'), 'true');
  await peer.close();
});

test('unsupported credential/transport fields fail rather than get silently dropped', () => {
  const { runtime } = fixture();
  assert.throws(() => registerProvider(runtime, 0, 'pi-local-provider', { ...definition(() => {}), oauth: {} }), /unsupported/);
  assert.throws(() => registerProvider(runtime, 0, 'pi-local-provider', { ...definition(() => {}), baseUrl: 'https://example.invalid/?api_key=secret' }), /unsupported/);
  assert.throws(() => registerProvider(runtime, 0, 'pi-local-provider', { ...definition(() => {}), streamSimple: undefined }), /unsupported/);
});

// workflow reads these in session_start: Pi's registry answers synchronously
// with every extension-registered provider and the config it was given.
test('modelRegistry lists registered provider ids and returns their configs', async () => {
  const value = await registered(() => {});
  const registry = modelRegistry(() => ({}), value.runtime);
  assert.deepEqual(registry.getRegisteredProviderIds(), ['pi-local-provider']);
  assert.equal(registry.getRegisteredProviderConfig('pi-local-provider').baseUrl, 'http://127.0.0.1:9/local/');
  assert.equal(registry.getRegisteredProviderConfig('missing'), undefined);
  unregisterProvider(value.runtime, 0, 'pi-local-provider'); await Promise.all(value.pending);
  assert.deepEqual(registry.getRegisteredProviderIds(), []);
});

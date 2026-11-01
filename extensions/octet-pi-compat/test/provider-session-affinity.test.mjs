import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { registerProvider, startProviderRegistration } from '../lib/providers.mjs';
import { providerState } from '../lib/custom-provider.mjs';
import { openaiCompletionsDelegate, openaiResponsesDelegate } from '../lib/native-provider.mjs';
import { owner, host } from './helper.mjs';

const deferred = () => { let resolve; const promise = new Promise(yes => { resolve = yes; }); return { promise, resolve }; };
const request = session_id => ({ system: 'native system', messages: [], tools: [], tool_choice: 'auto', stop: [], output_format: { type: 'text' }, reasoning: { type: 'off' }, max_output_tokens: 32, cache_retention: 'short', session_id });
const response = { message: { model: 'custom/model', protocol: 'open_ai_chat', content: [{ Text: 'native reply' }] }, usage: { input_tokens: 1, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: 1 }, stop_reason: 'end_turn', timestamp: 123 };
const affinityNames = ['session_id', 'x-client-request-id', 'x-session-affinity'];

async function fixture(t, { affinity = true, format, headers, modelHeaders, changeSession, pause, api = 'openai-completions', retired = false } = {}) {
  const frames = [], fetches = [], optionsSeen = [], delegated = [], terminals = new Map();
  let next = 10, runtime;
  const model = { id: 'custom/model', name: 'Custom', provider: 'affinity-fixture', api, baseUrl: 'https://custom.invalid/v1', reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 8192, maxTokens: 1024, ...(modelHeaders ? { headers: modelHeaders } : {}) };
  async function call(method, params) {
    const id = next++;
    await runtime.receive({ jsonrpc: '2.0', id, method, params });
    const reply = frames.find(frame => frame.id === id);
    assert.ok(reply, 'actual Runtime must settle the native caller');
    if (reply.error) throw Object.assign(new Error(reply.error.message), { code: reply.error.code });
    return reply.result;
  }
  runtime = new Runtime({ extensions: ['/affinity-fixture.mjs'] }, {
    async send(frame) { frames.push(frame); },
    async settleParent(id, error) { frames.push({ settled: id, error }); },
    async notify(method, params) {
      frames.push({ method, params });
      if (method === 'provider/event' && ['finished', 'error'].includes(params.kind)) terminals.get(params.stream_id)?.resolve(params.kind);
    },
    async request(method, params) {
      frames.push({ method, params });
      if (method === 'providers/register') return { revision: 1 };
      assert.equal(method, 'provider/delegate');
      delegated.push(structuredClone(params));
      if (pause) await pause.promise;
      await call('provider/delegate_http', { stream_id: params.stream_id, method: 'POST', url: `https://custom.invalid/v1/${api === 'openai-completions' ? 'chat/completions' : 'responses'}`, headers: { 'content-type': 'application/json' }, body: JSON.stringify({ model: model.id, stream: true, ...(api === 'openai-completions' ? { messages: [] } : { input: [] }) }) });
      const result = structuredClone(response);
      if (api === 'openai-responses') result.message.protocol = 'open_ai_responses';
      return result;
    },
  });
  runtime.uninstallChildren();
  runtime.features = new Set(['provider_proxy_v1', 'provider_inheritance_v1']);
  const store = { id: 1, method: 'hook/run', hook: 'session_start', live: true, controller: new AbortController(), pending: new Set(), errors: [] };
  runtime.bind({ context: { resource_owner: owner, workspace: '/work', host } }, store, { foreground: true });
  registerProvider(runtime, 0, { id: model.provider, getModels: () => [model], auth: { apiKey: { resolve: () => ({ auth: { apiKey: 'fixture-only' } }) } }, stream() { assert.fail('full stream is not invoked'); },
    streamSimple(selected, context, options) {
      optionsSeen.push({ sessionId: options.sessionId, cacheRetention: options.cacheRetention });
      const delegate = api === 'openai-completions' ? openaiCompletionsDelegate : openaiResponsesDelegate;
      return delegate({ ...selected, compat: { sendSessionAffinityHeaders: affinity, ...(format ? { sessionAffinityFormat: format } : {}) } }, context, { ...options,
        ...(changeSession === undefined ? {} : { sessionId: changeSession }),
        headers: { ...(options.sessionId ? { 'x-coral-session': options.sessionId } : {}), ...options.headers, ...headers },
        async fetch(url, init) { fetches.push({ url, headers: new Headers(init.headers), body: init.body }); return new Response(null); },
      });
    },
  });
  runtime.initialized = true;
  startProviderRegistration(runtime);
  await providerState(runtime).tail;
  if (retired) await runtime.retire(runtime.foreground);
  const wire = frames.find(frame => frame.method === 'providers/register').params.models[0];
  t.after(() => { runtime.stopping = true; for (const job of providerState(runtime).jobs.values()) job.controller.abort(); });
  async function run(session, id = `affinity-${next}`) {
    const terminal = deferred(); terminals.set(id, terminal);
    const input = request(session);
    assert.deepEqual(await call('provider/stream', { stream_id: id, provider_id: model.provider, model_id: wire.id, request: input }), { stream_id: id, accepted: true });
    return { id, input, terminal: terminal.promise };
  }
  return { runtime, frames, fetches, optionsSeen, delegated, run };
}

test('real Runtime keeps admitted cache identity stable and projects the explicit Pi affinity opt-in', { timeout: 2000 }, async t => {
  const value = await fixture(t);
  for (const session of ['native-cache-scope', 'native-cache-scope', 'next-cache-scope']) {
    const stream = await value.run(session);
    assert.equal(await stream.terminal, 'finished', JSON.stringify(value.frames));
    assert.equal(value.optionsSeen.at(-1).sessionId, stream.input.session_id);
    assert.equal(value.delegated.at(-1).request.session_id, stream.input.session_id);
    assert.equal(value.delegated.at(-1).request.cache_retention, stream.input.cache_retention);
    assert.notEqual(stream.input.session_id, host.session_id);
    assert.notEqual(stream.input.session_id, owner.session_id);
    const headers = value.fetches.at(-1).headers;
    assert.equal(headers.get('accept'), 'application/json');
    assert.equal(headers.get('x-coral-session'), stream.input.session_id);
    for (const name of affinityNames) assert.equal(headers.get(name), stream.input.session_id, name);
  }
});

test('absent admitted identity never falls back to retained owner or host session facts', { timeout: 2000 }, async t => {
  const value = await fixture(t);
  const stream = await value.run(null);
  assert.equal(await stream.terminal, 'finished');
  assert.equal(value.optionsSeen[0].sessionId, undefined);
  for (const name of [...affinityNames, 'x-coral-session']) assert.equal(value.fetches[0].headers.get(name), null);
  assert.equal(value.delegated[0].request.session_id, null);
});

test('a custom callback cannot substitute another session identity before native delegation', { timeout: 2000 }, async t => {
  const value = await fixture(t, { changeSession: 'different-cache-scope' });
  const stream = await value.run('native-cache-scope');
  assert.equal(await stream.terminal, 'error');
  assert.equal(value.delegated.length, 0);
  assert.equal(value.fetches.length, 0);
});

test('explicit affinity opt-out does not emit default OpenAI affinity headers', { timeout: 2000 }, async t => {
  const value = await fixture(t, { affinity: false });
  const stream = await value.run('native-cache-scope');
  assert.equal(await stream.terminal, 'finished');
  for (const name of affinityNames) assert.equal(value.fetches[0].headers.get(name), null);
});

test('OpenRouter affinity uses its pinned Pi convention rather than OpenAI headers', { timeout: 2000 }, async t => {
  const value = await fixture(t, { format: 'openrouter' });
  const stream = await value.run('native-cache-scope');
  assert.equal(await stream.terminal, 'finished');
  assert.equal(value.fetches[0].headers.get('x-session-id'), stream.input.session_id);
  for (const name of affinityNames) assert.equal(value.fetches[0].headers.get(name), null);
});

test('request headers retain precedence over affinity defaults and model headers', { timeout: 2000 }, async t => {
  const value = await fixture(t, { modelHeaders: { 'X-Session-Affinity': 'model-value' }, headers: { 'X-SESSION-AFFINITY': 'request-value' } });
  const stream = await value.run('native-cache-scope');
  assert.equal(await stream.terminal, 'finished');
  assert.equal(value.fetches[0].headers.get('x-session-affinity'), 'request-value');
  assert.equal(value.fetches[0].headers.get('session_id'), stream.input.session_id);
});

test('native stream cancellation before HTTP dispatch cannot disclose affinity identity', { timeout: 2000 }, async t => {
  const pause = deferred(), value = await fixture(t, { pause });
  const stream = await value.run('native-cache-scope');
  for (let n = 0; n < 20 && !value.delegated.length; n++) await new Promise(resolve => setImmediate(resolve));
  assert.equal(value.delegated.length, 1);
  await value.runtime.notify({ method: 'provider/cancel', params: { stream_id: stream.id } });
  pause.resolve();
  for (let n = 0; n < 4; n++) await new Promise(resolve => setImmediate(resolve));
  assert.equal(value.fetches.length, 0);
});

test('retired retained owner facts cannot supply a missing admitted cache identity', { timeout: 2000 }, async t => {
  const value = await fixture(t, { retired: true });
  assert.equal(value.runtime.foreground.alive, false);
  const stream = await value.run(null);
  assert.equal(await stream.terminal, 'finished');
  assert.equal(value.optionsSeen[0].sessionId, undefined);
  for (const name of [...affinityNames, 'x-coral-session']) assert.equal(value.fetches[0].headers.get(name), null);
});

test('Pi openai-nosession affinity omits session_id but keeps the two routing headers', { timeout: 2000 }, async t => {
  const value = await fixture(t, { format: 'openai-nosession' });
  const stream = await value.run('native-cache-scope');
  assert.equal(await stream.terminal, 'finished');
  assert.equal(value.fetches[0].headers.get('session_id'), null);
  for (const name of affinityNames.slice(1)) assert.equal(value.fetches[0].headers.get(name), stream.input.session_id);
});

test('Responses follows its own Pi session-header default without Chat affinity opt-in', { timeout: 2000 }, async t => {
  const value = await fixture(t, { api: 'openai-responses', affinity: false });
  const stream = await value.run('native-cache-scope');
  assert.equal(await stream.terminal, 'finished');
  for (const name of affinityNames.slice(0, 2)) assert.equal(value.fetches[0].headers.get(name), stream.input.session_id);
  assert.equal(value.fetches[0].headers.get('x-session-affinity'), null);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { prepareCustomProviderHttp, openCustomProviderHttp, pullCustomProviderHttp, closeCustomProviderHttp } from '../lib/custom-provider-http.mjs';

const model = { id: 'custom-model', provider: 'custom', api: 'openai-completions', baseUrl: 'https://custom.invalid/v1/' };
function fixture(fetch) {
  const job = { controller: new AbortController(), entry: { custom: {} }, delegated: true };
  prepareCustomProviderHttp(job, model, { fetch, apiKey: 'process-only-fixture' });
  return job;
}
const request = () => ({ stream_id: 'stream', method: 'POST', url: 'https://custom.invalid/v1/chat/completions', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ model: model.id, stream: true, messages: [] }) });

test('custom fetch stays inert until admitted opening and pulls bounded owned bytes', async () => {
  let calls = 0, observed;
  const bytes = new Uint8Array(70000).fill(42);
  const job = fixture(async (url, init) => {
    calls++; observed = { url, init };
    return new Response(new ReadableStream({ start(controller) { controller.enqueue(bytes); controller.close(); } }), { headers: { 'content-type': 'text/event-stream' } });
  });
  assert.equal(calls, 0);
  assert.equal((await openCustomProviderHttp(job, request())).status, 200);
  assert.equal(observed.init.headers.Authorization, 'Bearer process-only-fixture');
  assert.equal(observed.url, request().url);
  assert.deepEqual(JSON.parse(observed.init.body), JSON.parse(request().body));
  const first = Buffer.from((await pullCustomProviderHttp(job, { stream_id: 'stream' })).chunk, 'base64');
  const second = Buffer.from((await pullCustomProviderHttp(job, { stream_id: 'stream' })).chunk, 'base64');
  assert.equal(first.length, 65536);
  assert.equal(second.length, 4464);
  assert.deepEqual(Buffer.concat([first, second]), Buffer.from(bytes));
  assert.deepEqual(await pullCustomProviderHttp(job, { stream_id: 'stream' }), { done: true });
  await assert.rejects(openCustomProviderHttp(job, request()), /one fetch/);
  assert.equal(calls, 1);
  closeCustomProviderHttp(job);
});

test('OpenRouter session affinity uses the endpoint hostname or explicit provider identity', () => {
  for (const [provider, baseUrl, expected] of [
    ['custom', 'https://openrouter.ai/api/v1', true],
    ['custom', 'https://API.OPENROUTER.AI/v1', true],
    ['openrouter', 'https://custom.invalid/v1', true],
    ['custom', 'https://openrouter.ai.invalid/v1', false],
    ['custom', 'https://not-openrouter.ai/v1', false],
    ['custom', 'https://custom.invalid/openrouter.ai/v1', false],
  ]) {
    const job = { controller: new AbortController() };
    prepareCustomProviderHttp(job, { ...model, provider, baseUrl }, { sessionId: 'fixture-session', fetch() { assert.fail('preparation must not send HTTP'); } });
    assert.equal(job.customHttp.headers['x-session-id'], expected ? 'fixture-session' : undefined, baseUrl);
    closeCustomProviderHttp(job);
  }
});

test('custom bridge rejects changed endpoint/model and host auth before fetch', async () => {
  let calls = 0;
  for (const changed of [
    { url: 'https://other.invalid/v1/chat/completions' },
    { method: 'GET' },
    { headers: { Authorization: 'native-fixture-secret' } },
    { body: JSON.stringify({ model: 'other', stream: true }) },
    { body: JSON.stringify({ model: model.id, stream: false }) },
  ]) {
    const job = fixture(async () => { calls++; return new Response(''); });
    await assert.rejects(openCustomProviderHttp(job, { ...request(), ...changed }));
    closeCustomProviderHttp(job);
  }
  assert.equal(calls, 0);
});

test('cancellation aborts a quiet custom reader and refuses subsequent pull', async () => {
  let cancelled = false;
  const job = fixture(async () => new Response(new ReadableStream({ cancel() { cancelled = true; } })));
  await openCustomProviderHttp(job, request());
  const pulling = pullCustomProviderHttp(job, { stream_id: 'stream' });
  job.controller.abort();
  await assert.rejects(pulling);
  assert.equal(cancelled, true);
  await assert.rejects(pullCustomProviderHttp(job, { stream_id: 'stream' }));
});

test('one reader rejects concurrent pulls without dispatching another fetch', async () => {
  const job = fixture(async () => new Response(new ReadableStream()));
  await openCustomProviderHttp(job, request());
  const pulling = pullCustomProviderHttp(job, { stream_id: 'stream' });
  await assert.rejects(pullCustomProviderHttp(job, { stream_id: 'stream' }), /one reader/);
  job.controller.abort();
  await assert.rejects(pulling);
});

test('native jobs and unadmitted custom jobs cannot obtain HTTP transport', async () => {
  for (const mutate of [job => { job.entry.inherited = true; }, job => { job.entry.custom = undefined; }, job => { job.delegated = false; }]) {
    const job = fixture(async () => { assert.fail('not admitted'); });
    mutate(job);
    await assert.rejects(openCustomProviderHttp(job, request()), /live admitted custom delegate/);
    closeCustomProviderHttp(job);
  }
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { launch, root, owner, noPiAgent } from './helper.mjs';

// One stalled factory must never wedge the ordered hook queue, the prompt, or
// resource discovery. The budget is shortened for the suite; the product
// default is longer (see lib/handler-budget.mjs).
const stalled = join(root, 'test/fixtures/stalled-lifecycle.ts');
const healthy = join(root, 'test/fixtures/prompt-suffix.ts');
const options = { env: { OCTET_PI_HANDLER_BUDGET_MS: '300' } };
const features = ['before_prompt_state_v1'];
const start = peer => peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
const prompt = (peer, text) => ({ hook: 'before_prompt', payload: { prompt: text, system_prompt: 'base' }, context: peer.context() });

test('a session_start handler that never settles is detached instead of wedging the hook queue', async t => {
  const peer = launch(t, [stalled, healthy], options); await peer.init(features);
  const started = await start(peer).response;
  assert.ok(started.result, JSON.stringify(started));
  const begun = Date.now();
  const composed = await peer.request('hook/run', prompt(peer, 'hello')).response;
  assert.equal(composed.result?.system_prompt, 'base\nhealthy', JSON.stringify(composed));
  assert.ok(Date.now() - begun < 3000, 'the prompt hook answers within the host budget');
  await peer.close();
});

test('a detached handler is reported once per event, naming its factory, then skipped while it stays busy', async t => {
  const peer = launch(t, [stalled, healthy], { env: { OCTET_PI_HANDLER_BUDGET_MS: '600' } }); await peer.init(features);
  const reportFor = event => peer.wait(frame => frame.method === 'notification' && /stalled-lifecycle\.ts/.test(frame.params?.message ?? '') && frame.params.message.includes(event));
  await start(peer).response;
  const session = await reportFor('session_start');
  assert.match(session.params.message, /stopped waiting/);
  await peer.request('hook/run', prompt(peer, 'one')).response;
  await reportFor('before_agent_start');
  // Still stalled: the next prompt skips it at once and says nothing more.
  const begun = Date.now();
  const composed = await peer.request('hook/run', prompt(peer, 'two')).response;
  assert.equal(composed.result?.system_prompt, 'base\nhealthy', JSON.stringify(composed));
  assert.ok(Date.now() - begun < 400, `a busy handler is skipped, not waited on again (${Date.now() - begun} ms)`);
  await assert.rejects(peer.wait(frame => frame.method === 'notification' && /stalled-lifecycle\.ts/.test(frame.params?.message ?? ''), 800));
  await peer.close();
});

test('a stalled tool_call guard fails closed instead of wedging the bridge', async t => {
  const peer = launch(t, [stalled], options); await peer.init(features);
  await start(peer).response;
  const guarded = await peer.request('hook/run', { hook: 'before_tool_call', payload: { name: 'bash', tool_call_id: 'call-1', arguments: { command: 'true' } }, context: peer.context() }).response;
  assert.equal(guarded.result?.disposition?.action, 'deny', JSON.stringify(guarded));
  assert.match(guarded.result.disposition.reason, /did not answer/);
  await peer.close();
});

const stalledPipeline = join(root, 'test/fixtures/stalled-pipeline.ts');
const pipelineFeatures = ['resource_paths_v1', 'pipeline_hooks_v1', 'session_entries'];

test('a resources_discover callback that never settles contributes no paths and does not hold discovery', async t => {
  const peer = launch(t, [stalledPipeline], options); await peer.init(pipelineFeatures); await peer.start();
  const reply = await peer.request('hook/run', { hook: 'resources_discover', payload: { cwd: peer.context().workspace, reason: 'startup' }, context: peer.context() }).response;
  assert.deepEqual(reply.result, { resource_paths: { skill_paths: [], prompt_paths: [], theme_paths: [] } }, JSON.stringify(reply));
  await peer.close();
});

test('a before_provider_request callback that never settles leaves the payload untouched', async t => {
  const peer = launch(t, [stalledPipeline], options); await peer.init(pipelineFeatures); await peer.start();
  const payload = { model: 'test-model', messages: [] };
  const reply = await peer.request('hook/run', { hook: 'before_provider_request', payload: { operation_id: 'op-1', model: { id: 'test-model', provider: 'test', api: 'openai-responses' }, payload }, context: peer.context() }).response;
  assert.deepEqual(reply.result, { provider_payload: payload }, JSON.stringify(reply));
  await peer.close();
});

// A factory's own async setup is awaited at load, ahead of every hook. The host
// waits 30 s for the whole load, so one that never settles must be skipped.
const stalledFactory = join(root, 'test/fixtures/stalled-factory.ts');
const inspectWith = (extensions, env) => spawnSync(process.execPath, [join(root, 'runner.mjs'), '--inspect', ...extensions], { encoding: 'utf8', timeout: 10000, env: { ...process.env, OCTET_PI_AGENT_DIR: noPiAgent, ...env } });

test('a factory whose setup never settles is skipped at load while the others still register', async t => {
  const begun = Date.now();
  const result = inspectWith([stalledFactory, healthy], { OCTET_PI_HANDLER_BUDGET_MS: '300' });
  assert.equal(result.status, 0, `${result.error ?? ''}${result.stderr}`);
  assert.ok(Date.now() - begun < 5000, 'load finishes within a budget, not the host deadline');
  const metadata = JSON.parse(result.stdout).result;
  assert.deepEqual(metadata.commands, [], 'the stalled factory contributes nothing, not even what it registered before stalling');
  assert.ok(metadata.hooks.includes('before_prompt'), 'the healthy factory still loads');
  assert.match(result.stderr, /stalled-factory\.ts: .*did not finish within/);
});

test('a stalled factory is reported by name when the bridge starts', async t => {
  const metadata = JSON.parse(inspectWith([stalledFactory, healthy], { OCTET_PI_HANDLER_BUDGET_MS: '300' }).stdout).result;
  const peer = launch(t, [stalledFactory, healthy], { metadata, env: { OCTET_PI_HANDLER_BUDGET_MS: '300' } }); await peer.init(features);
  const issue = await peer.wait(frame => frame.method === 'notification' && /stalled-factory\.ts/.test(frame.params?.message ?? ''));
  assert.match(issue.params.message, /still setting itself up|did not finish/);
  assert.match(issue.params.message, /Next:/);
  await peer.close();
});

test('a lone explicit entrypoint that never settles still fails, so configure can exclude it', async t => {
  const result = inspectWith([stalledFactory], { OCTET_PI_HANDLER_BUDGET_MS: '300' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /setup did not finish within/);
});

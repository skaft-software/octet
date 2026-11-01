import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { launch, root, owner, noPiAgent } from './helper.mjs';
import { HookBudget, budgetFor } from '../lib/handler-budget.mjs';
import { Runtime } from '../lib/runtime.mjs';
import { transformInput } from '../lib/context-api.mjs';

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

test('a module whose top-level await never settles is skipped like a stalled factory', async t => {
  const stalledImport = join(root, 'test/fixtures/stalled-import.mjs');
  const begun = Date.now();
  const result = inspectWith([stalledImport, healthy], { OCTET_PI_HANDLER_BUDGET_MS: '300' });
  assert.equal(result.status, 0, `${result.error ?? ''}${result.stderr}`);
  assert.ok(Date.now() - begun < 5000);
  assert.ok(JSON.parse(result.stdout).result.hooks.includes('before_prompt'), 'the healthy factory still loads');
  assert.match(result.stderr, /stalled-import\.mjs: .*did not finish within/);
});

test('many stalled hook handlers share one deadline and do not start after it', async () => {
  const previous = process.env.OCTET_PI_HANDLER_BUDGET_MS;
  process.env.OCTET_PI_HANDLER_BUDGET_MS = '25';
  try {
    const reports = [];
    const runtime = { config: { extensions: [] }, issues: { slow: (event, items) => reports.push({ event, items }) } };
    const store = { pending: new Set(), errors: [] };
    const budget = new HookBudget(runtime, store, 'input');
    const begun = Date.now();
    let invoked = 0;
    for (let index = 0; index < 30; index += 1) {
      const result = await budget.run({ label: `handler-${index}` }, { ...store }, () => {
        invoked += 1;
        return new Promise(() => {});
      });
      assert.equal(result.ran, false);
    }
    budget.finish();
    assert.ok(Date.now() - begun < 450, `handler floors must not accumulate (${Date.now() - begun} ms)`);
    assert.equal(budget.wait(), 0);
    assert.ok(invoked < 30, 'no new handler starts once the shared deadline is exhausted');
    assert.equal(reports.length, 1);
    assert.equal(reports[0].event, 'input');
    assert.ok(reports[0].items.length > 0);
  } finally {
    if (previous === undefined) delete process.env.OCTET_PI_HANDLER_BUDGET_MS;
    else process.env.OCTET_PI_HANDLER_BUDGET_MS = previous;
  }
});

test('a healthy late handler can use the shared reserve without extending the deadline', async () => {
  const runtime = { config: { extensions: [] }, issues: { slow() {} } };
  const store = { pending: new Set(), errors: [] };
  const budget = new HookBudget(runtime, store, 'input');
  budget.perHandler = 100; budget.total = 160;
  const stalled = await budget.run({ label: 'stalled' }, { ...store }, () => new Promise(() => {}));
  assert.equal(stalled.ran, false);
  const healthy = await budget.run({ label: 'healthy' }, { ...store }, () => 'kept');
  assert.deepEqual(healthy, { ran: true, value: 'kept' });
  budget.finish();
});

test('a later timeout does not discard an earlier tracked host-operation failure', async () => {
  const previous = process.env.OCTET_PI_HANDLER_BUDGET_MS;
  process.env.OCTET_PI_HANDLER_BUDGET_MS = '70';
  const failure = new Error('earlier host mutation failed');
  let rejectHost;
  const pendingHost = new Promise((_, reject) => { rejectHost = reject; });
  const runtime = new Runtime({ extensions: ['/earlier.ts', '/later.ts'], pi_agent_dir: noPiAgent }, {
    request: () => pendingHost,
    notify: async () => {},
  });
  runtime.uninstallChildren();
  runtime.issues.slow = () => {};
  runtime.features.add('composer');
  const store = { id: 1, method: 'hook/run', controller: new AbortController(), pending: new Set(), errors: [], live: true };
  runtime.active.set(store.id, store);
  runtime.bind({ context: { resource_owner: owner, workspace: process.cwd(), host: { project_trusted: false } } }, store, { foreground: true });
  let laterCalled = false;
  runtime.events.set('input', [
    { factory: 0, handler: (_event, ctx) => { void ctx.ui.setEditorText('draft'); } },
    { factory: 1, handler: () => { laterCalled = true; rejectHost(failure); return new Promise(() => {}); } },
  ]);
  try {
    await assert.rejects(transformInput(runtime, { phase: 'input', text: 'hello', source: 'interactive' }, store), error => error === failure);
    assert.equal(laterCalled, true);
    assert.deepEqual(store.errors, [failure]);
  } finally {
    if (previous === undefined) delete process.env.OCTET_PI_HANDLER_BUDGET_MS;
    else process.env.OCTET_PI_HANDLER_BUDGET_MS = previous;
  }
});

function trackedHostFixture() {
  let rejectHost;
  const pendingHost = new Promise((_, reject) => { rejectHost = reject; });
  const runtime = new Runtime({ extensions: ['/first.ts', '/second.ts'], pi_agent_dir: noPiAgent }, {
    request: () => pendingHost, notify: async () => {},
  });
  runtime.uninstallChildren();
  runtime.issues.slow = () => {};
  runtime.features.add('composer');
  const store = { id: 1, method: 'hook/run', controller: new AbortController(), pending: new Set(), errors: [], live: true };
  runtime.active.set(store.id, store);
  runtime.bind({ context: { resource_owner: owner, workspace: process.cwd(), host: { project_trusted: false } } }, store, { foreground: true });
  return { runtime, store, rejectHost };
}

const inputPayload = { phase: 'input', text: 'hello', source: 'interactive' };

test('a timed-out callback cannot erase its own already-recorded host failure', async () => {
  const { runtime, store, rejectHost } = trackedHostFixture();
  const failure = new Error('timed-out handler host mutation failed');
  runtime.events.set('input', [{ factory: 0, handler: (_event, ctx) => {
    void ctx.ui.setEditorText('draft');
    rejectHost(failure);
    return new Promise(() => {});
  } }]);
  const previous = process.env.OCTET_PI_HANDLER_BUDGET_MS;
  process.env.OCTET_PI_HANDLER_BUDGET_MS = '40';
  try {
    await assert.rejects(transformInput(runtime, inputPayload, store), error => error === failure);
    assert.deepEqual(store.errors, [failure]);
  } finally {
    if (previous === undefined) delete process.env.OCTET_PI_HANDLER_BUDGET_MS;
    else process.env.OCTET_PI_HANDLER_BUDGET_MS = previous;
  }
});

test('a tracked operation rejected after timeout still fails the live input boundary', async () => {
  const { runtime, store, rejectHost } = trackedHostFixture();
  const failure = new Error('late host mutation failed');
  runtime.events.set('input', [
    { factory: 0, handler: (_event, ctx) => { void ctx.ui.setEditorText('draft'); return new Promise(() => {}); } },
    { factory: 1, handler: () => { rejectHost(failure); } },
  ]);
  const previous = process.env.OCTET_PI_HANDLER_BUDGET_MS;
  process.env.OCTET_PI_HANDLER_BUDGET_MS = '40';
  try {
    await assert.rejects(transformInput(runtime, inputPayload, store), error => error === failure);
    assert.deepEqual(store.errors, [failure]);
  } finally {
    if (previous === undefined) delete process.env.OCTET_PI_HANDLER_BUDGET_MS;
    else process.env.OCTET_PI_HANDLER_BUDGET_MS = previous;
  }
});

test('a previously tracked host failure after the boundary becomes a background error', async () => {
  const { runtime, store, rejectHost } = trackedHostFixture();
  const failure = new Error('host mutation failed after boundary');
  const background = [];
  runtime.backgroundError = error => background.push(error);
  runtime.events.set('input', [{ factory: 0, handler: (_event, ctx) => {
    void ctx.ui.setEditorText('draft');
    return new Promise(() => {});
  } }]);
  const previous = process.env.OCTET_PI_HANDLER_BUDGET_MS;
  process.env.OCTET_PI_HANDLER_BUDGET_MS = '40';
  try {
    assert.deepEqual(await transformInput(runtime, inputPayload, store), { action: 'continue' });
    store.live = false;
    runtime.active.delete(store.id);
    rejectHost(failure);
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(background, [failure]);
    assert.deepEqual(store.errors, []);
  } finally {
    if (previous === undefined) delete process.env.OCTET_PI_HANDLER_BUDGET_MS;
    else process.env.OCTET_PI_HANDLER_BUDGET_MS = previous;
  }
});

test('cancellation interrupts a waiting callback and does not start the next handler', async () => {
  const { runtime, store } = trackedHostFixture();
  let ready, settle, nextCalled = false;
  const started = new Promise(resolve => { ready = resolve; });
  runtime.events.set('input', [
    { factory: 0, handler: () => { ready(); return new Promise(resolve => { settle = resolve; }); } },
    { factory: 1, handler: () => { nextCalled = true; } },
  ]);
  const reason = new Error('input cancelled');
  const operation = transformInput(runtime, inputPayload, store);
  await started;
  store.controller.abort(reason);
  await assert.rejects(operation, error => error === reason);
  settle();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(nextCalled, false);
});

test('resource discovery leaves time before the native five-second deadline', () => {
  const { perHandler, total } = budgetFor('resources_discover');
  assert.ok(perHandler > 0 && perHandler <= total);
  assert.ok(total <= 4000, `discovery aggregate ${total} ms must leave native processing time`);
});

test('many stalled factory setups end the load by one absolute deadline, not one floor each', async () => {
  const previous = process.env.OCTET_PI_HANDLER_BUDGET_MS;
  process.env.OCTET_PI_HANDLER_BUDGET_MS = '50';
  try {
    const { LoadBudget } = await import('../lib/handler-budget.mjs');
    const budget = new LoadBudget(); // 50 ms each, 200 ms total, 250 ms deadline
    const begun = Date.now();
    let stalledCount = 0;
    for (let index = 0; index < 20; index += 1) {
      try { await budget.settle(() => new Promise(() => {})); }
      catch (error) { assert.equal(error.name, 'FactoryStalled'); stalledCount += 1; }
    }
    assert.equal(stalledCount, 20);
    // Without the shared deadline each setup past the total still waited its
    // 50 ms floor: 20 stalls took about 1000 ms.
    assert.ok(Date.now() - begun < 600, `stalls are bounded by the absolute deadline (${Date.now() - begun} ms)`);
    assert.equal(budget.wait(), 0, 'nothing is left to wait once the deadline has passed');
    assert.equal(await budget.settle(() => 'healthy'), 'healthy', 'a synchronous setup still registers');
  } finally {
    if (previous === undefined) delete process.env.OCTET_PI_HANDLER_BUDGET_MS;
    else process.env.OCTET_PI_HANDLER_BUDGET_MS = previous;
  }
});

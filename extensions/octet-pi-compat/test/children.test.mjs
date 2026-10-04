import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { createAgentSession, SessionManager, readTool, writeTool, createReadTool, withChildHost, CHILD_FEATURES, DEFAULT_CHILD_LIMITS, retireChildSessions } from '../lib/children.mjs';
import { ChildEventProjection, childUsage } from '../lib/child-events.mjs';
import { parseChildArgv, runChildCli } from '../lib/child-cli.mjs';

const owner = { session_id: 'issued-owner', extension_instance_id: 'issued-instance', process_generation: 3 };
const usage = { input_tokens: 20, output_tokens: 3, cache_read_tokens: 4, cache_write_tokens: 2, total_tokens: 29 };
const cost = { input: 20, output: 6, reasoning: 2, cache_read: 1, cache_write: 2, total: 31, total_picodollars_remainder: 500000 };
const event = (kind, payload = {}) => ({ kind, timestamp: 1000, ...payload });
function runEvents(message = 'task', answer = 'answer') {
  return [event('run_started', { message }), event('turn_started'), event('output_delta', { channel: 'text', text: answer }),
    event('turn_finished', { message: { content: [{ Text: answer }], model: 'native-model', protocol: 'open_ai_responses' }, usage, cost, stop_reason: 'EndTurn' }), event('run_finished', { reason: 'completed' })];
}
function host(handler) {
  let sequence = 0, pending = [], live = true;
  const requests = [], identity = {};
  const binding = { identity, owner, cwd: '/host/workspace', features: new Set([...CHILD_FEATURES, 'agent_model_selection_v1']),
    assertLive() { if (!live) throw new Error('host owner revoked'); },
    async request(method, params) {
      requests.push({ method, params: structuredClone(params) });
      if (handler) return handler(method, params, binding);
      if (method === 'agent/spawn' || method === 'agent/follow_up') {
        pending = runEvents(params.message).map(event => ({ sequence: ++sequence, event }));
        return { agent_id: 'native-child', policy: params.policy, resolved_model: { model: 'native-model', provider: 'native-provider' } };
      }
      if (method === 'agent/events') { const events = pending; pending = []; return { agent_id: 'native-child', session_id: 'native-session-id', events, next_sequence: sequence, has_more: false, status: { state: 'completed' } }; }
      if (method === 'agent/stop') return { agent_id: 'native-child', shutdown_requested: true };
      if (method === 'agent/interrupt') return { interrupt_requested: true };
      if (method === 'agent/message') return { accepted: true };
      assert.fail(`unexpected ${method}`);
    },
  };
  return { binding, requests, revoke() { live = false; }, run: fn => withChildHost(binding, fn) };
}
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }

// All provider/host responses below are deterministic local protocol fixtures.
// They qualify projection/control semantics, not the native integration binding.
test('SDK runs and continues one host-owned session with finite limits and real observations', async () => {
  const h = host();
  await h.run(async () => {
    const manager = SessionManager.inMemory('/host/workspace');
    const { session } = await createAgentSession({ sessionManager: manager, tools: [readTool, writeTool] });
    assert.equal(h.requests.length, 0, 'construction starts no hidden agent');
    assert.equal(manager.getSessionFile(), undefined);
    assert.throws(() => manager.getSessionId(), /native session identity/);
    const events = [], unsubscribe = session.subscribe(e => events.push(e));
    await session.prompt('inspect it');
    assert.equal(session.sessionId, 'native-session-id');
    assert.equal(session.sessionFile, undefined);
    assert.deepEqual(events.map(e => e.type), ['agent_start', 'turn_start', 'message_start', 'message_update', 'message_end', 'turn_end', 'agent_end']);
    assert.deepEqual(h.requests[0].params.policy, { ...DEFAULT_CHILD_LIMITS, tools: ['read', 'write'] });
    assert.equal(h.requests[0].params.policy.max_depth, 1, 'nested callback paths are explicitly unsupported, not granted implicitly');
    assert.equal(events[4].message.usage.input, 20);
    assert.equal(events[4].message.usage.cost.total, 0.0000315);
    assert.equal(events[4].message.api, 'openai-responses');
    assert.equal(session.messages[1].content[0].text, 'answer');
    session.messages[1].content[0].text = 'not canonical';
    assert.equal(session.messages[1].content[0].text, 'answer');
    unsubscribe();
    await session.prompt('continue');
    assert.equal(events.length, 7);
    assert.equal(h.requests.filter(r => r.method === 'agent/spawn').length, 1);
    assert.equal(h.requests.filter(r => r.method === 'agent/follow_up').length, 1);
    assert.equal(session.messages.length, 4);
    await session.dispose(); await session.dispose();
    assert.equal(h.requests.filter(r => r.method === 'agent/stop').length, 1);
    await assert.rejects(session.prompt('late'), /disposed/);
  });
});

test('tool callbacks follow committed assistant and actual native tool settlement', () => {
  const seen = [], p = new ChildEventProjection(e => seen.push(e));
  p.accept(event('run_started', { message: 'read' })); p.accept(event('turn_started'));
  p.accept(event('turn_finished', { message: { content: [{ ToolCall: { id: 'call-1', name: 'read', arguments_json: '{"path":"a"}' } }], model: 'native', protocol: 'open_ai_responses' }, usage, cost, stop_reason: 'ToolUse' }));
  assert.ok(!seen.some(e => e.type === 'turn_end'));
  p.accept(event('tool_started', { id: 'call-1', name: 'read', arguments: { path: 'a' } }));
  p.accept(event('tool_finished', { id: 'call-1', content: [{ type: 'text', text: 'actual result' }], metadata: { pi_details: { checked: true } }, is_error: false }));
  assert.deepEqual(seen.slice(-4).map(e => e.type), ['tool_execution_start', 'tool_execution_end', 'message_end', 'turn_end']);
  assert.deepEqual(seen.at(-1).toolResults[0].details, { checked: true });
  p.accept(event('run_finished', { reason: 'completed' })); p.settled();
  assert.equal(seen.at(-1).type, 'agent_end');
  assert.throws(() => p.settled(), /without a native run terminal/);
});

test('callbacks cannot turn an unpriced turn or dropped stream into fabricated success', () => {
  assert.throws(() => childUsage(usage, null), /fictional Pi zero/);
  const p = new ChildEventProjection(() => {});
  assert.throws(() => p.accept(event('output_discarded')), /discarded by native host/);
  assert.throws(() => p.accept(event('observation_error', { error: 'cursor overflow' })), /cursor overflow/);
});

test('sequence gap fails and stops exactly that child without replaying spawn', async () => {
  const h = host((method) => {
    if (method === 'agent/spawn') return { agent_id: 'native-child', policy: { ...DEFAULT_CHILD_LIMITS, tools: ['read', 'bash', 'edit', 'write'] } };
    if (method === 'agent/events') return { agent_id: 'native-child', events: [{ sequence: 2, event: event('run_started', { message: 'x' }) }], next_sequence: 2, has_more: false, status: { state: 'completed' } };
    if (method === 'agent/stop') return { shutdown_requested: true };
    assert.fail(method);
  });
  await h.run(async () => { const { session } = await createAgentSession(); await assert.rejects(session.prompt('x'), /sequence gap/); });
  assert.deepEqual(h.requests.map(r => r.method), ['agent/spawn', 'agent/events', 'agent/stop']);
});

test('no negotiated lifetime binding or owner means no spawn', async () => {
  await assert.rejects(createAgentSession(), /live authenticated host child binding/);
  const h = host(); h.binding.features.delete('agent_session_lifetime_v1');
  await h.run(() => assert.rejects(createAgentSession(), /agent_session_lifetime_v1/));
  assert.equal(h.requests.length, 0);
});

test('retained SDK handles never retarget a replacement owner', async () => {
  const h = host(); let session;
  await h.run(async () => { ({ session } = await createAgentSession()); await session.prompt('x'); });
  h.revoke();
  await assert.rejects(session.steer('late'), /owner revoked/);
  const before = h.requests.length;
  retireChildSessions(undefined, h.binding.identity);
  await assert.rejects(session.abort(), /disposed/);
  assert.equal(h.requests.length, before);
});

test('dispose racing spawn waits for the admitted child identity and sends one stop', async () => {
  const start = deferred();
  const h = host(method => {
    if (method === 'agent/spawn') return start.promise;
    if (method === 'agent/stop') return { shutdown_requested: true };
    assert.fail(method);
  });
  await h.run(async () => {
    const { session } = await createAgentSession();
    const prompt = assert.rejects(session.prompt('x'), /disposed while spawning/);
    const dispose = session.dispose();
    start.resolve({ agent_id: 'native-child', policy: { ...DEFAULT_CHILD_LIMITS, tools: ['read', 'bash', 'edit', 'write'] } });
    await Promise.all([prompt, dispose]);
    assert.deepEqual(h.requests.map(r => r.method), ['agent/spawn', 'agent/stop']);
  });
});

test('abort racing spawn cannot return before native interrupt admission', async () => {
  const start = deferred(), observations = deferred(), interrupt = deferred();
  const h = host(method => {
    if (method === 'agent/spawn') return start.promise;
    if (method === 'agent/events') return observations.promise;
    if (method === 'agent/interrupt') { interrupt.resolve(); return { interrupt_requested: true }; }
    if (method === 'agent/stop') return { shutdown_requested: true };
    assert.fail(method);
  });
  await h.run(async () => {
    const { session } = await createAgentSession(); const prompt = session.prompt('x');
    const abort = session.abort(); start.resolve({ agent_id: 'native-child', policy: { ...DEFAULT_CHILD_LIMITS, tools: ['read', 'bash', 'edit', 'write'] } });
    await interrupt.promise; await abort;
    const events = [event('run_started', { message: 'x' }), event('run_finished', { reason: 'interrupted' })].map((event, i) => ({ sequence: i + 1, event }));
    observations.resolve({ agent_id: 'native-child', events, next_sequence: 2, has_more: false, status: { state: 'interrupted' } });
    await prompt; await session.dispose();
    assert.equal(h.requests.filter(r => r.method === 'agent/interrupt').length, 1);
  });
});

test('an ambiguous native spawn is not retried or reported as a successful disposal', async () => {
  const h = host(() => { throw new Error('accepted response lost'); });
  await h.run(async () => {
    const { session } = await createAgentSession();
    await assert.rejects(session.prompt('x'), /response lost/);
    await assert.rejects(session.dispose(), /response lost/);
  });
  assert.equal(h.requests.length, 1);
});

test('unsupported Pi authority/file/callback paths are explicit before any spawn', async () => {
  const h = host();
  await h.run(async () => {
    for (const options of [{ customTools: [{ name: 'nested_agent' }] }, { cwd: '/elsewhere' }, { resourceLoader: {} }, { settingsManager: {} }, { modelRuntime: {} }, { agentDir: '/pi' }, { modelRegistry: {} }, { tools: [{ name: 'read', execute() {} }] }]) {
      await assert.rejects(createAgentSession(options), /unsupported_feature/);
    }
    assert.throws(() => SessionManager.open('/fake/session.jsonl'), /implicit import/);
    assert.throws(() => SessionManager.create(), /projection is not implemented/);
    assert.throws(() => createReadTool('/elsewhere'), /inherit/);
    const { session } = await createAgentSession();
    assert.throws(() => { session.agent.beforeToolCall = () => {}; }, /effect admission/);
    await assert.rejects(session.bindExtensions({}), /not yet bound/);
    await session.dispose();
  });
  assert.equal(h.requests.length, 0);
});

test('CLI accepted argv drives the same host service and real JSON events', async () => {
  const h = host(), events = [];
  assert.equal(await runChildCli(['--mode', 'json', '-p', '--no-session', '--tools', 'read,search', 'inspect'], { binding: h.binding, emit: e => events.push(e) }), 0);
  assert.equal(events.at(-1).type, 'agent_end');
  assert.deepEqual(h.requests[0].params.policy.tools, ['read', 'search']);
  assert.equal(h.requests.at(-1).method, 'agent/stop');
  assert.deepEqual(parseChildArgv(['--mode', 'json', '-p', '--model', 'provider/model', '--thinking', 'high', 'x']).options.model, { provider: 'provider', id: 'model' });
  for (const argv of [['--mode', 'rpc', '-p', 'x'], ['--mode', 'json', '-p', '--session', '/fake', 'x'], ['--mode', 'json', '-p', '--append-system-prompt', 'secret', 'x'], ['--mode', 'json', '-p', '@task.txt']]) assert.throws(() => parseChildArgv(argv), /unsupported_feature/);
});

test('package-root CLI never starts a hidden upstream agent without authenticated launch', () => {
  const env = { ...process.env }; delete env.OCTET_PI_CHILD_SOCKET; delete env.OCTET_PI_CHILD_TOKEN;
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('../child-sdk/dist/cli.js', import.meta.url)), '--mode', 'json', '-p', 'x'], { env, encoding: 'utf8', timeout: 5000 });
  assert.equal(result.status, 1); assert.equal(result.stdout, ''); assert.match(result.stderr, /owner-bound octet launch bridge is required/);
});

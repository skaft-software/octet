import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Runtime } from '../lib/runtime.mjs';
import { rpcError, strict } from '../lib/errors.mjs';
import { launch, owner, root } from './helper.mjs';

const issueNotice = frame => frame.method === 'notification' && frame.params.title === '[Extension issues]';
const secret = 'PRIVATE_PROMPT_DO_NOT_REPORT';
function fixtures(t, sources) {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-issues-')));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return Object.entries(sources).map(([name, source]) => {
    const path = join(directory, name + '.ts'); writeFileSync(path, source); return path;
  });
}
function harness(t, extensions = ['/reviewed/first.ts', '/reviewed/second.ts']) {
  const frames = [], diagnostics = [];
  t.mock.method(console, 'error', (...args) => diagnostics.push(args.join(' ')));
  const runtime = new Runtime({ extensions }, {
    closed: false,
    async notify(method, params) { frames.push({ method, params }); },
    fail(error) { assert.fail(error); },
  });
  t.after(() => runtime.uninstallChildren?.());
  const state = { owner, alive: true, host: {}, workspace: root };
  const store = { id: 1, state, controller: new AbortController(), pending: new Set(), errors: [], live: true };
  runtime.foreground = state; runtime.active.set(store.id, store);
  return { runtime, frames, diagnostics, store };
}

test('startup groups skipped factories and inert renderers into one actionable Extension issues notice', async t => {
  const entries = fixtures(t, {
    render: `export default pi => {
      pi.registerMessageRenderer('notice', () => { throw new Error('inert renderer ran'); });
      pi.registerEntryRenderer('saved', () => undefined);
      pi.registerCommand('again', { handler: () => pi.registerMessageRenderer('later', () => undefined) });
    };`,
    broken: `export default () => { throw new Error('${secret}'); };`,
  });
  entries.push(join(entries[0], '..', 'missing.ts'));
  const peer = launch(t, entries);
  await peer.init(['remote_ui']);
  assert.ok((await peer.command('again').response).result); // Ordered barrier after initialize and late registration.
  const notices = peer.seen.filter(issueNotice);
  assert.equal(notices.length, 1, JSON.stringify(peer.seen) + '\n' + peer.stderr());
  assert.equal(notices[0].params.level, 'warning');
  for (const entry of entries) assert.ok(notices[0].params.message.includes(entry), entry);
  assert.match(notices[0].params.message, /failed while loading/i);
  assert.match(notices[0].params.message, /not found/i);
  assert.match(notices[0].params.message, /default transcript view/i);
  assert.equal((notices[0].params.message.match(/Next:/g) || []).length, 3);
  assert.doesNotMatch(notices[0].params.message, /unsupported_feature|invalid_request|ENOENT/);
  assert.doesNotMatch(peer.stderr(), new RegExp(secret + '|inert renderer ran'));
  assert.equal(peer.seen.filter(frame => frame.method === 'notification').length, 1);
  await peer.close();
});

test('late renderer issues name their extension once; supported and headless renderers stay silent', async t => {
  const entries = fixtures(t, { late: `export default pi => pi.registerCommand('install', {handler: () => {
    pi.registerMessageRenderer('late', () => undefined);
  }});` });
  for (const features of [['remote_ui'], ['remote_ui', 'transcript_render_v1'], []]) {
    const peer = launch(t, entries); await peer.init(features);
    for (let n = 0; n < 2; n++) assert.ok((await peer.command('install').response).result);
    const notices = peer.seen.filter(issueNotice);
    assert.equal(notices.length, features.length === 1 ? 1 : 0);
    if (notices.length) { assert.ok(notices[0].params.message.includes(entries[0])); assert.match(notices[0].params.message, /Next:/); }
    await peer.close();
  }
});

test('real callback requests continue after errors, report once, and report again after process restart', async t => {
  const entries = fixtures(t, { callback: `export default pi => {
    pi.on('after_response', event => { throw new Error(event.response); });
    pi.on('after_response', (_event, ctx) => ctx.ui.notify('later callback ran'));
  };` });
  for (let generation = 0; generation < 2; generation++) {
    const peer = launch(t, entries); await peer.init([]); await peer.start();
    for (let turn = 0; turn < 3; turn++) {
      const reply = await peer.request('hook/run', { hook: 'after_response', payload: { response: secret }, context: peer.context() }).response;
      assert.deepEqual(reply.result, { disposition: { action: 'continue' }, context: [], notifications: [] });
    }
    const notices = peer.seen.filter(issueNotice);
    assert.equal(notices.length, 1);
    assert.ok(notices[0].params.message.includes(entries[0]));
    assert.match(notices[0].params.message, /after_response/);
    assert.equal(peer.seen.filter(frame => frame.method === 'notification' && frame.params.message === 'later callback ran').length, 3);
    assert.doesNotMatch(peer.stderr(), new RegExp(secret));
    await peer.close();
  }
});

test('callback errors are redacted and reported once per extension/event, not per handler, error text, or turn', async t => {
  const { runtime, store, frames, diagnostics } = harness(t);
  let attempts = 0, continued = 0;
  const fails = () => { attempts++; throw new Error(`${secret}:${attempts}`); };
  runtime.events.set('agent_end', [{ factory: 0, handler: fails }, { factory: 0, handler: fails },
    { factory: 1, handler: fails }, { factory: 1, handler: () => { continued++; } }]);
  runtime.events.set('message_end', [{ factory: 0, handler: fails }]);
  for (let turn = 0; turn < 3; turn++) await runtime.runEvent('agent_end', { type: 'agent_end' }, store);
  await runtime.runEvent('message_end', { type: 'message_end' }, store);
  assert.equal(attempts, 10, 'report suppression must not disable callbacks');
  assert.equal(continued, 3, 'later handlers must still execute');
  assert.equal(frames.length, 3); assert.ok(frames.every(issueNotice));
  assert.equal(frames.filter(frame => frame.params.message.includes('/reviewed/first.ts') && frame.params.message.includes('agent_end')).length, 1);
  assert.equal(frames.filter(frame => frame.params.message.includes('/reviewed/second.ts') && frame.params.message.includes('agent_end')).length, 1);
  assert.equal(frames.filter(frame => frame.params.message.includes('message_end')).length, 1);
  for (const frame of frames) {
    assert.equal(frame.params.level, 'warning');
    assert.match(frame.params.message, /callback failed/i); assert.match(frame.params.message, /Next:/);
    assert.match(frame.params.message, /update|disable/i);
  }
  assert.equal(diagnostics.length, 3, 'stderr must not repeat callback failures either');
  assert.doesNotMatch(JSON.stringify([frames, diagnostics]), new RegExp(secret));
  store.state = { ...store.state, owner: { ...owner, session_id: 'next-session' } }; runtime.foreground = store.state;
  await runtime.runEvent('agent_end', { type: 'agent_end' }, store);
  assert.equal(frames.length, 3, 'new session owners do not reset the process issue ledger');
});

test('unsupported callback fields have a plain reason without echoing a private computed property', async t => {
  const { runtime, store, frames, diagnostics } = harness(t);
  runtime.events.set('agent_end', [{ factory: 0, handler: event => event[secret] }]);
  await runtime.runEvent('agent_end', strict({ type: 'agent_end' }, 'agent_end event'), store);
  assert.equal(frames.length, 1); assert.ok(issueNotice(frames[0]));
  assert.match(frames[0].params.message, /API or event field.*not support/i);
  assert.match(frames[0].params.message, /Next:/);
  assert.doesNotMatch(JSON.stringify([frames, diagnostics]), new RegExp(secret + '|unsupported_feature'));
});

test('tool-result callback failures are redacted and deduplicated while later transformations still run', async t => {
  const { runtime, store, frames, diagnostics } = harness(t);
  runtime.events.set('tool_result', [
    { factory: 0, handler: event => { throw new Error(event.content[0].text); } },
    { factory: 1, handler: () => ({ content: [{ type: 'text', text: 'replacement' }] }) },
  ]);
  for (let n = 0; n < 3; n++) {
    assert.deepEqual(await runtime.runToolResult({ name: 'read', arguments: {}, output: secret, is_error: false }, store), { content: ['replacement'] });
  }
  assert.equal(frames.length, 1); assert.ok(issueNotice(frames[0]));
  assert.match(frames[0].params.message, /tool_result/);
  assert.ok(frames[0].params.message.includes('/reviewed/first.ts'));
  assert.equal(diagnostics.length, 1);
  assert.doesNotMatch(JSON.stringify([frames, diagnostics]), new RegExp(secret));
});

test('veto, cancellation, owner fences, and tracked host mutations still fail closed without a skipped-callback notice', async t => {
  const { runtime, store, frames } = harness(t);
  const veto = new Error('permission callback failed');
  runtime.events.set('tool_call', [{ factory: 0, handler: () => { throw veto; } }]);
  await assert.rejects(runtime.runEvent('tool_call', {}, store, { veto: true }), error => error === veto);
  for (const event of ['agent_end', 'tool_result']) {
    for (const code of [-32800, -32002]) {
      const refusal = rpcError(code, 'refused');
      runtime.events.set(event, [{ factory: 0, handler: () => { throw refusal; } }]);
      await assert.rejects(event === 'tool_result' ? runtime.runToolResult({ name: 'read', output: '', arguments: {} }, store)
        : runtime.runEvent(event, {}, store), error => error === refusal);
    }
    const refused = rpcError(-32006, 'host mutation refused');
    runtime.events.set(event, [{ factory: 0, handler: async () => { await runtime.track(Promise.reject(refused)); } }]);
    await assert.rejects(event === 'tool_result' ? runtime.runToolResult({ name: 'read', output: '', arguments: {} }, store)
      : runtime.runEvent(event, {}, store), error => error === refused);
    await assert.rejects(runtime.flush(store), error => error === refused);
    store.errors.length = 0;
  }
  assert.equal(frames.length, 0);
});

test('grouped startup diagnostics do not silently truncate later paths and safely escape terminal controls', t => {
  const paths = Array.from({ length: 64 }, (_, n) => `/reviewed/${'long-name-'.repeat(12)}${n}.ts`);
  paths[0] = '/reviewed/\x1b[31mprivate\npath.ts';
  const { runtime, frames, diagnostics } = harness(t, paths);
  runtime.loadFailures = paths.map(entry => ({ entry, error: secret, code: 'ENOENT' }));
  runtime.reportStartupIssues(); runtime.reportStartupIssues();
  assert.equal(frames.length, 1); assert.ok(issueNotice(frames[0]));
  for (const path of paths.slice(1)) assert.ok(frames[0].params.message.includes(path), path);
  assert.match(frames[0].params.message, /\\u001b\[31mprivate\\u000apath\.ts/);
  assert.doesNotMatch(JSON.stringify([frames, diagnostics]), new RegExp(secret));
  assert.doesNotMatch(frames[0].params.message, /\x1b/);
});

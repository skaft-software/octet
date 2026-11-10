import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Runtime } from '../lib/runtime.mjs';
import { createContext } from '../lib/api.mjs';
import { rpcError } from '../lib/errors.mjs';
import { launch, owner, root } from './helper.mjs';

const issueNotice = frame => frame.method === 'notification' && frame.params.title === '[Extension issues]';
function harness(t, extensions = ['/reviewed/first.ts']) {
  const frames = [], diagnostics = [];
  t.mock.method(console, 'error', (...args) => diagnostics.push(args.join(' ')));
  const runtime = new Runtime({ extensions }, {
    closed: false,
    // A host that serves no such method: the real -32601 a factory sees.
    request(method) { return Promise.reject(rpcError(-32601, `method not found: ${method}`)); },
    async notify(method, params) { frames.push({ method, params }); },
    fail(error) { assert.fail(error); },
  });
  t.after(() => runtime.uninstallChildren?.());
  const state = { owner, alive: true, host: {}, workspace: root };
  const store = { id: 1, state, controller: new AbortController(), pending: new Set(), errors: [], live: true };
  runtime.foreground = state; runtime.active.set(store.id, store);
  return { runtime, frames, diagnostics, store };
}

test('a hook that calls an unsupported Pi member names that member in the notice and the log', t => {
  const { runtime, frames, diagnostics, store } = harness(t);
  runtime.events.set('session_start', [{ factory: 0, handler: (_event, ctx) => ctx.abort() }]);
  return runtime.runEvent('session_start', { type: 'session_start' }, store).then(() => {
    assert.equal(frames.length, 1); assert.ok(issueNotice(frames[0]));
    assert.equal(frames[0].params.level, 'warning');
    assert.equal(frames[0].params.message,
      'First: session_start hook needs ctx.abort, which octet does not provide, so the hook was skipped. Update octet or the extension, or disable.');
    assert.match(diagnostics[0], /session_start hook needs ctx\.abort/);
  });
});

test('an unknown host method names the method, not the hook event, and never claims an event field', async t => {
  const { runtime, frames, diagnostics, store } = harness(t, ['/reviewed/pi-web-access.ts']);
  runtime.events.set('session_start', [{ factory: 0, handler: () => runtime.hostCall('session/set_leaf', {}, runtime.scope.getStore()) }]);
  await runtime.runEvent('session_start', { type: 'session_start' }, store);
  assert.equal(frames.length, 1);
  assert.equal(frames[0].params.message,
    'Pi Web Access: session_start hook needs session/set_leaf, which octet does not provide, so the hook was skipped. Update octet or the extension, or disable.');
  assert.doesNotMatch(JSON.stringify([frames, diagnostics]), /event field/);
});

test('a session-view refusal names the session getter the hook needed', async t => {
  const { runtime, frames, store } = harness(t);
  runtime.events.set('session_start', [{ factory: 0, handler: (_event, ctx) => {
    store.state.host.session_branch = null; // The host published "unavailable".
    return ctx.sessionManager.getBranch();
  } }]);
  await runtime.runEvent('session_start', { type: 'session_start' }, store);
  assert.equal(frames.length, 1);
  assert.match(frames[0].params.message, /^First: session_start hook needs ctx\.sessionManager, which octet does not provide/);
});

test('a refusal about a member the factory merely probed stays unnamed and keeps its own text out of every notice', async t => {
  const { runtime, frames, diagnostics, store } = harness(t);
  const secret = 'PRIVATE_PROMPT_DO_NOT_REPORT';
  runtime.events.set('session_start', [{ factory: 0, handler: (_event, ctx) => ctx[secret] = 'x' }]);
  await runtime.runEvent('session_start', { type: 'session_start' }, store);
  assert.equal(frames.length, 1);
  assert.match(frames[0].params.message, /^First: session_start hook called an API octet does not support, so the hook was skipped/);
  assert.doesNotMatch(JSON.stringify([frames, diagnostics]), new RegExp(secret + '|unsupported_feature|event field'));
});

test('a package names the skipped hook, the member it needed, and the one action', async t => {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-member-')));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  writeFileSync(join(directory, 'package.json'), JSON.stringify({ name: 'pi-web-access', pi: { displayName: 'Pi Web Access' } }));
  writeFileSync(join(directory, 'index.ts'), 'export default pi => { pi.on(\'session_start\', (_event, ctx) => ctx.abort()); };');
  const peer = launch(t, [join(directory, 'index.ts')]);
  await peer.init([]);
  await peer.start();
  const notices = peer.seen.filter(issueNotice);
  assert.equal(notices.length, 1, peer.stderr());
  assert.equal(notices[0].params.message,
    'Pi Web Access: session_start hook needs ctx.abort, which octet does not provide, so the hook was skipped. Update octet or the extension, or disable.');
  assert.ok(!notices[0].params.message.includes(directory), 'a notice names no path');
  await peer.waitStderr(/session_start hook needs ctx\.abort/);
  await peer.close();
});

test('a probing factory that sees undefined members runs to completion with no notice', async t => {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-probe-')));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const out = join(directory, 'probe.json');
  writeFileSync(join(directory, 'probe.ts'), `import { writeFileSync } from 'node:fs';
export default pi => {
  pi.on('session_start', (event, ctx) => {
    const absent = [ctx.goalStorageRoot, ctx.sessionManager.getEntryCount, ctx.ui.customFooterWidget,
      event.futureSessionField, pi.futureRegistration];
    if (absent.some(member => member !== undefined)) throw new Error('a probed member must read as absent');
    if (typeof ctx.sessionManager.getBranch !== 'function') throw new Error('real members stay present');
    writeFileSync(${JSON.stringify(out)}, JSON.stringify({ probed: absent.length }));
  });
};`);
  const peer = launch(t, [join(directory, 'probe.ts')]);
  await peer.init();
  const started = await peer.start({ session_entries: [], session_branch: [], session_leaf_id: null });
  assert.deepEqual(started.result.disposition, { action: 'continue' });
  assert.deepEqual(JSON.parse(readFileSync(out, 'utf8')), { probed: 5 });
  assert.equal(peer.seen.filter(issueNotice).length, 0, peer.stderr());
  assert.doesNotMatch(peer.stderr(), /probed member|real members/);
  await peer.close();
});

test('a session_start history read before the host publishes its session view uses Pi empty-session shape', async t => {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-fresh-')));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const out = join(directory, 'history.json');
  writeFileSync(join(directory, 'history.ts'), `import { writeFileSync } from 'node:fs';
export default pi => {
  pi.on('session_start', (_event, ctx) => {
    writeFileSync(${JSON.stringify(out)}, JSON.stringify({
      entries: ctx.sessionManager.getEntries(),
      branch: ctx.sessionManager.getBranch(),
      file: ctx.sessionManager.getSessionFile(),
      leaf: ctx.sessionManager.getLeafId(),
      header: ctx.sessionManager.getHeader(),
      label: ctx.sessionManager.getLabel('entry-1'),
    }));
  });
};`);
  const peer = launch(t, [join(directory, 'history.ts')]);
  await peer.init();
  // No session_entries/session_branch/session_file/session_leaf_id: the first
  // boundary of a session, before any mirror publication.
  const started = await peer.start({ project_trusted: true });
  assert.deepEqual(started.result.disposition, { action: 'continue' });
  assert.deepEqual(JSON.parse(readFileSync(out, 'utf8')),
    { entries: [], branch: [], leaf: null, header: null });
  assert.equal(peer.seen.filter(issueNotice).length, 0, peer.stderr());
  await peer.close();
});

test('a published unavailable session view still refuses its getters by name', t => {
  const { runtime, store } = harness(t);
  runtime.bind({ context: { workspace: '/workspace', resource_owner: owner, host: {} } }, store, { foreground: true });
  const manager = createContext(runtime, store).sessionManager;
  assert.deepEqual(manager.getEntries(), [], 'no publication yet: Pi empty session');
  assert.deepEqual(manager.getBranch(), []);
  assert.equal(manager.getSessionFile(), undefined);
  store.state.host.session_entries = null;
  store.state.host.session_branch = null;
  assert.throws(() => manager.getEntries(), error => error.code === -32601
    && /unsupported_feature ctx\.sessionManager: native session view unavailable/.test(error.message));
});

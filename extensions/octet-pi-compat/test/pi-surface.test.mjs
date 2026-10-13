// Pi 1.0.2 public module surface named by the real-extension matrix capture
// blockers: `pi-subagents` (keyText), `@gotgenes/pi-permission-system`
// (getPackageDir) and `pi-fabric` (CURRENT_SESSION_VERSION), plus the
// absent-member semantics those packages rely on.
import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { createContext } from '../lib/api.mjs';
import { Runtime } from '../lib/runtime.mjs';
import { CURRENT_SESSION_VERSION, getPackageDir, getPackageJsonPath, getReadmePath } from '../lib/pi-config.mjs';
import { facade } from '../lib/errors.mjs';
import { launch, owner, root } from './helper.mjs';

test('Pi package/config helpers report the package that serves the module', () => {
  const dir = getPackageDir();
  assert.equal(dir, root.replace(/\/$/, ''));
  assert.equal(getPackageJsonPath(), `${dir}/package.json`);
  assert.equal(getReadmePath(), `${dir}/README.md`);
  assert.ok(existsSync(getPackageJsonPath()) && existsSync(getReadmePath()));
  assert.equal(JSON.parse(readFileSync(getPackageJsonPath(), 'utf8')).name, '@skaft-software/octet-pi-compat');
  // Pi 1.0.2's session-record version, not a guess from another release.
  assert.equal(CURRENT_SESSION_VERSION, 3);
});

test('keybinding hint helpers use the host bindings and refuse an unpublished action', async t => {
  const entry = `${root}test/fixtures/keybinding-hints.ts`;
  const peer = launch(t, [entry]);
  await peer.init([]);
  const result = await peer.request('command/execute', {
    name: 'hints', arguments: [], context: peer.context(),
  }).response;
  assert.ok(result.result, JSON.stringify(result));
  const proof = peer.seen.find(frame => frame.method === 'notification' && frame.params.message?.startsWith('{'));
  assert.ok(proof, JSON.stringify(peer.seen.slice(-8)));
  const report = JSON.parse(proof.params.message);
  assert.equal(report.keyText, 'enter');
  assert.equal(report.refused, true, JSON.stringify(report));
  assert.match(report.rawHint, /close/);
  assert.match(report.hint, /quit/);
  // Pi's own helpers colorize with safe SGR, exactly as the pinned theme does.
  assert.match(report.rawHint, /\u001b\[[0-9;]*m/);
  assert.equal(report.keyTextError, undefined);
  await peer.close();
});

test('facade: an undefined member is absent, an assignment is refused', () => {
  const object = { defined: 1 };
  const pi = facade(object, 'ctx');
  assert.equal(pi.defined, 1);
  assert.equal(pi.goalStorageRoot, undefined);
  assert.equal(pi['lsp:workspace-provider'], undefined);
  assert.equal('goalStorageRoot' in pi, false);
  assert.equal(pi.goalStorageRoot?.(), undefined);
  assert.throws(() => { pi.goalStorageRoot = '/tmp'; }, /unsupported_feature ctx.goalStorageRoot/);
  assert.equal(object.goalStorageRoot, undefined);
  // An event payload keeps a defined field readable and its mutation observable,
  // exactly like the plain object Pi hands a callback.
  const event = facade({ type: 'session_start', reason: 'startup' }, 'session_start event');
  assert.equal(event.reason, 'startup');
  event.reason = 'resume';
  assert.equal(event.reason, 'resume');
  assert.equal(event.sessionFile, undefined);
});

test('ctx is a facade: optional members are absent while refusing operations still throw', t => {
  const runtime = new Runtime({ extensions: [] }, {});
  t.after(() => runtime.uninstallChildren());
  const state = { owner, alive: true, workspace: '/native-workspace', host: { session_id: 's', session_entries: [], session_branch: [], statuses: new Map() } };
  const store = { id: 1, state, controller: new AbortController() };
  runtime.foreground = state;
  const ctx = createContext(runtime, store);
  assert.equal(ctx.goalStorageRoot, undefined, 'optional member probe from another Pi release');
  assert.equal(ctx.futurePrivateMethod, undefined);
  assert.equal(typeof ctx.abort, 'function');
  assert.throws(() => ctx.abort(), /unsupported_feature ctx.abort/);
  assert.throws(() => ctx.shutdown(), /unsupported_feature ctx.shutdown/);
  assert.throws(() => ctx.ui.onTerminalInput(), /unsupported_feature terminal_input_intercept_v1/);
});

test('footer data is a read-only facade whose optional private mutators are absent', async t => {
  const runtime = new Runtime({extensions: []}, {});
  t.after(() => runtime.uninstallChildren());
  const state = {owner, alive: true, workspace: root, host: {}, statuses: new Map([['fixture', 'ready']]), uiQueues: new Map()};
  const store = {id: 1, state, controller: new AbortController(), pending: new Set(), errors: []};
  runtime.foreground = state; runtime.features.add('remote_ui');
  runtime.ui.mount = async (_store, _placement, _title, make) => make({}, {});
  const ctx = createContext(runtime, store);
  await ctx.ui.setFooter((_tui, _theme, data) => {
    assert.equal(data.setExtensionStatus, undefined);
    assert.equal(data.clearExtensionStatuses, undefined);
    assert.equal(typeof data.onBranchChange, 'function');
    assert.deepEqual(data.getExtensionStatuses(), state.statuses);
    assert.throws(() => { data.setExtensionStatus = () => {}; }, /unsupported_feature/);
    return {render: () => [], invalidate() {}};
  });
});

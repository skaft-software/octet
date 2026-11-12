import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createContext } from '../lib/api.mjs';
import { RemoteUI } from '../lib/remote-ui.mjs';
import { Timers } from '../lib/timers.mjs';

test('footer closed during readiness cannot construct a timer-registering factory', async t => {
  const workspace = await mkdtemp(join(tmpdir(), 'octet-footer-ready-'));
  t.after(() => rm(workspace, { recursive: true, force: true }));
  const state = { workspace, key: 'owner', alive: true, host: {}, uiQueues: new Map(), statuses: new Map(), branchListeners: new Set() };
  const store = { id: 7, factory: 0, state, controller: new AbortController() };
  const errors = [], frames = [];
  const runtime = {
    scope: new AsyncLocalStorage(), foreground: state, features: new Set(['remote_ui']),
    require(feature) { assert.ok(this.features.has(feature)); },
    assertOwner(value) { assert.equal(value.state, state); assert.ok(state.alive); },
    assertSessionOwner(value) { this.assertOwner(value); },
    backgroundError(error) { errors.push(error); },
    terminalInput: { add() {} },
    transport: { async notify(method, params) { frames.push({ method, params }); } },
    async hostCall(method) {
      assert.ok(['ui/open', 'ui/close'].includes(method));
      return method === 'ui/open' ? { columns: 80, rows: 24 } : {};
    },
  };
  const timers = runtime.timers = new Timers(runtime);
  const originals = Object.fromEntries(['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'].map(key => [key, globalThis[key]]));
  timers.install();
  t.after(() => { timers.all(); Object.assign(globalThis, originals); });
  const ui = runtime.ui = new RemoteUI(runtime);
  t.after(() => ui.shutdown());
  const mount = ui.mount.bind(ui);
  ui.mount = (owner, placement, title, factory, options) => mount(owner, placement, title, (...args) => {
    const constructing = factory(...args);
    const surface = runtime.scope.getStore().surface;
    assert.ok(surface.footerData, 'close after readiness starts but before it settles');
    const closing = ui.close(surface, undefined, true);
    return Promise.all([constructing, closing]).then(([component]) => component);
  }, options);
  let calls = 0;
  const ctx = createContext(runtime, store);
  const mounting = ctx.ui.setFooter(() => {
    calls++;
    setInterval(() => {}, 1000);
    return { render: () => ['closed footer'], invalidate() {} };
  });
  await mounting;
  assert.equal(timers.handles.size, 0, 'closed surfaces must not acquire timers after cleanup');
  assert.equal(calls, 0, 'the extension factory must not run after closure');
  assert.deepEqual(errors, []);
  assert.deepEqual(frames, []);
});

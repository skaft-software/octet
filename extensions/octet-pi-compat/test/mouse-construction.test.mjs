import test, { afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';

const enable = '\x1b[?1000h\x1b[?1002h\x1b[?1006h';
const disable = '\x1b[?1000l\x1b[?1002l\x1b[?1006l';
const owner = { session_id: 'mouse-test', extension_instance_id: 'pi', process_generation: 1 };
let currentRuntime;
afterEach(() => currentRuntime?.uninstallChildren?.());
function fixture({ holdOpen = 0, holdClose = false, refuseCapture = false, malformedOpen = 0, openBarrier } = {}) {
  const calls = [], mounts = new Map(), errors = [];
  const pending = Promise.withResolvers(), release = Promise.withResolvers();
  let opens = 0;
  const runtime = new Runtime({}, {
    async request(method, params) {
      assert.deepEqual(params.resource_owner, owner);
      assert.equal(params.parent_request_id, 1);
      calls.push({ method, params });
      const id = params.surface_id;
      if (method === 'ui/open') {
        assert.equal(mounts.has(id), false, 'duplicate host mount');
        if (refuseCapture && params.mouse_capture) throw new Error('capture refused');
        // Commit before replying, just like a host whose ACK is still in transit.
        mounts.set(id, Boolean(params.mouse_capture));
        if (++opens === holdOpen) { pending.resolve(); await release.promise; }
        await openBarrier?.(opens, params);
        return { columns: opens === malformedOpen ? 0 : opens === 1 ? 120 : 100, rows: opens === 1 ? 40 : 30 };
      }
      assert.equal(method, 'ui/close');
      assert.equal(mounts.delete(id), true, 'close of unknown host mount');
      if (holdClose) { pending.resolve(); await release.promise; }
      return {};
    },
    async notify(method, params) {
      assert.equal(method, 'ui/frame');
      assert.equal(mounts.has(params.surface_id), true, 'frame without host admission');
      calls.push({ method, params });
    },
  });
  currentRuntime = runtime;
  runtime.features.add('remote_ui');
  const state = { key: 'test', owner, alive: true };
  runtime.foreground = state;
  runtime.backgroundError = error => errors.push(error);
  const store = { state, factory: 0, id: 1, controller: new AbortController() };
  return { runtime, store, calls, mounts, errors, pending: pending.promise, release: release.resolve, refuse: release.reject };
}
function drawing(f, extra = {}) {
  let finish, tui, disposed = 0;
  const inputs = [];
  const mounting = f.runtime.ui.mount(f.store, 'fullscreen', 'test', (ui, _theme, _keys, done) => {
    tui = ui; finish = done;
    assert.deepEqual(tui.terminal.geometry, { columns: 120, rows: 40 });
    tui.terminal.write(enable);
    return { render: () => ['canvas'], handleInput: data => inputs.push(data), dispose() { disposed++; tui.terminal.write(disable); }, ...extra };
  });
  return { mounting, inputs, get tui() { return tui; }, done: value => finish(value), get disposed() { return disposed; } };
}
// Each pending RPC is an explicit barrier, not a scheduling delay/grace period.
const noFrames = f => assert.equal(f.calls.some(c => c.method === 'ui/frame'), false);
const mouse = { surface_id: 'pi-1', kind: 'press', button: 'left', x: 2, y: 3, modifiers: [] };

test('constructor capture is host-admitted before first frame, using actual geometry', async () => {
  const f = fixture(), d = drawing(f), surface = await d.mounting;
  assert.deepEqual(f.calls.map(c => c.method), ['ui/open', 'ui/close', 'ui/open', 'ui/frame']);
  assert.equal(f.calls[2].params.mouse_capture, true);
  assert.deepEqual(d.tui.terminal.geometry, { columns: 100, rows: 30 });
  f.runtime.ui.handle('ui/mouse', mouse);
  assert.deepEqual(d.inputs, ['\x1b[<0;3;4M']);
  d.tui.terminal.write(disable); // Teardown intent, not a local revocation of the host lease.
  f.runtime.ui.handle('ui/mouse', mouse);
  assert.equal(d.inputs.length, 2);
  await f.runtime.ui.close(surface);
  assert.equal(d.disposed, 1); assert.equal(f.mounts.size, 0); assert.equal(f.runtime.ui.surfaces.size, 0);
  assert.deepEqual(f.errors, []);
});

for (const refused of [false, true]) test('custom completion waits for host close ACK: ' + (refused ? 'refusal' : 'success'), async () => {
    const f = fixture({ holdClose: true }), results = [], errors = [];
    const surface = await f.runtime.ui.mount(f.store, 'fullscreen', 'test', () => ({ render: () => ['canvas'] }),
      { done: value => results.push(value), reject: error => errors.push(error.message) });
    const closing = f.runtime.ui.close(surface, 'saved');
    await f.pending;
    assert.deepEqual(results, [], 'continuation cannot paste while host close is pending');
    if (refused) {
      f.refuse(new Error('restoration refused'));
      await assert.rejects(closing, /restoration refused/);
      assert.deepEqual(results, []); assert.deepEqual(errors, ['restoration refused']);
    } else {
      f.release(); await closing;
      assert.deepEqual(results, ['saved']); assert.deepEqual(errors, []);
    }
});

test('capture refusal disposes exactly once without a frame or duplicate close', async () => {
  const f = fixture({ refuseCapture: true }), d = drawing(f);
  await assert.rejects(d.mounting, /capture refused/);
  noFrames(f); assert.equal(d.disposed, 1); assert.equal(f.mounts.size, 0); assert.equal(f.runtime.ui.surfaces.size, 0);
  assert.deepEqual(f.errors, []);
});

test('ordinary surfaces do not reopen and cannot acquire capture after construction', async () => {
  const f = fixture();
  const surface = await f.runtime.ui.mount(f.store, 'fullscreen', 'test', () => ({ render: () => ['plain'] }));
  assert.deepEqual(f.calls.map(c => c.method), ['ui/open', 'ui/frame']);
  assert.throws(() => surface.tui.terminal.write(enable), /changing live mouse capture/);
  assert.throws(() => f.runtime.ui.handle('ui/mouse', mouse), /without capture/);
  await f.runtime.ui.close(surface); assert.equal(f.mounts.size, 0);
});

test('done during re-admission open releases the late admitted host lease', async () => {
  const f = fixture({ holdOpen: 2 }), d = drawing(f);
  await f.pending; noFrames(f);
  await d.done('saved');
  assert.equal(f.runtime.ui.surfaces.size, 0); assert.equal(d.disposed, 1);
  f.release(); const surface = await d.mounting;
  assert.equal(surface.closed, true); assert.equal(f.mounts.size, 0, 'late ACK must not leak mount/capture');
  assert.equal(f.calls.filter(c => c.method === 'ui/close').length, 2);
  noFrames(f); assert.deepEqual(f.errors, []);
});

test('ordinary disable during pending capture admission reconciles before the first frame', async () => {
  const f = fixture({ holdOpen: 2 }), d = drawing(f);
  await f.pending; noFrames(f);
  // The prior assert.throws test proved refusal safety only, not this repair.
  d.tui.terminal.write(disable);
  f.release(); const surface = await d.mounting;
  assert.equal(f.mounts.get(surface.id), false);
  assert.equal(surface.desiredMouseCapture, false); assert.equal(surface.admittedMouseCapture, false);
  assert.deepEqual(f.calls.filter(c => c.method === 'ui/open').map(c => c.params.mouse_capture), [false, true, false]);
  assert.equal(f.calls.filter(c => c.method === 'ui/frame').length, 1);
  assert.throws(() => f.runtime.ui.handle('ui/mouse', mouse), /without capture/);
  await f.runtime.ui.close(surface); assert.equal(f.mounts.size, 0); assert.deepEqual(f.errors, []);
});

for (const stage of ['initial open', 're-admission close', 're-admission open']) {
  test(`done during ${stage} prevents publication and further admission`, async () => {
    const f = fixture(stage === 're-admission close' ? { holdClose: true } : { holdOpen: stage === 'initial open' ? 1 : 2 });
    const d = drawing(f);
    await f.pending;
    const surface = f.runtime.ui.surfaces.get('pi-1');
    const closing = f.runtime.ui.close(surface, 'saved');
    f.release(); await closing; await d.mounting;
    assert.equal(f.mounts.size, 0); assert.equal(f.runtime.ui.surfaces.size, 0); noFrames(f);
    assert.equal(f.calls.filter(c => c.method === 'ui/open').length, stage === 're-admission open' ? 2 : 1);
    assert.equal(d.disposed, stage === 'initial open' ? 0 : 1); assert.deepEqual(f.errors, []);
  });
}

for (const holdOpen of [2, 3]) for (const retirement of ['rescue', 'owner', 'shutdown', 'cancel']) {
  test(`${retirement} during pending re-admission ${holdOpen} cannot revive a surface`, async () => {
    const f = fixture({ holdOpen, openBarrier: open => {
      if (holdOpen === 3 && open === 2) f.runtime.ui.surfaces.get('pi-1').tui.terminal.write(disable);
    } }), d = drawing(f);
    await f.pending;
    if (retirement === 'cancel') await f.runtime.ui.cancelParent(1);
    else {
      f.mounts.clear(); // Host retirement, unlike local done, already restores its terminal.
      if (retirement === 'rescue') f.runtime.ui.handle('ui/closed', { surface_id: 'pi-1', reason: 'rescue' });
      if (retirement === 'owner') { f.store.state.alive = false; await f.runtime.ui.ownerEnded(f.store.state); }
      if (retirement === 'shutdown') { f.runtime.stopping = true; await f.runtime.ui.shutdown(); }
    }
    f.release(); const surface = await d.mounting;
    assert.equal(surface.opened, false); assert.equal(surface.admittedMouseCapture, false);
    noFrames(f); assert.equal(f.mounts.size, 0); assert.equal(f.runtime.ui.surfaces.size, 0); assert.equal(d.disposed, 1);
    // Owner revocation makes dispose's optional terminal write unavailable.
    if (retirement !== 'owner') assert.deepEqual(f.errors, []);
  });
}

test('oscillating intent across pending opens reconciles the final desired lease before activation', async () => {
  const gates = new Map(Array.from({ length: 4 }, (_, i) => [i + 2, { entered: Promise.withResolvers(), reply: Promise.withResolvers() }]));
  const f = fixture({ openBarrier: async (open, params) => {
    const gate = gates.get(open);
    if (gate) { gate.entered.resolve(params); await gate.reply.promise; }
  } });
  let detached = 0;
  f.store.method = 'command/execute'; f.store.detach = () => { detached++; };
  const d = drawing(f);
  for (const [open, desired] of [[2, false], [3, true], [4, false], [5, false]]) {
    const requested = await gates.get(open).entered.promise;
    noFrames(f); assert.equal(detached, 0);
    const surface = f.runtime.ui.surfaces.get('pi-1');
    assert.equal(surface.requestedMouseCapture, requested.mouse_capture);
    assert.equal(f.mounts.get(surface.id), requested.mouse_capture);
    d.tui.terminal.write(desired ? enable : disable);
    assert.equal(surface.desiredMouseCapture, desired);
    // Changing intent never rewrites the capture value in the in-flight request.
    assert.equal(surface.requestedMouseCapture, requested.mouse_capture);
    gates.get(open).reply.resolve();
  }
  const surface = await d.mounting;
  assert.equal(surface.admittedMouseCapture, false); assert.equal(surface.desiredMouseCapture, false);
  assert.equal(f.mounts.get(surface.id), false); assert.equal(detached, 1);
  assert.deepEqual(f.calls.filter(c => c.method === 'ui/open').map(c => c.params.mouse_capture), [false, true, false, true, false]);
  assert.equal(f.calls.filter(c => c.method === 'ui/frame').length, 1);
  await f.runtime.ui.close(surface); assert.deepEqual(f.errors, []);
});

test('oscillation returning to the in-flight requested value needs no extra admission', async () => {
  const f = fixture({ holdOpen: 2 }), d = drawing(f);
  await f.pending;
  d.tui.terminal.write(disable); d.tui.terminal.write(enable);
  noFrames(f); f.release(); const surface = await d.mounting;
  assert.equal(surface.desiredMouseCapture, true); assert.equal(surface.admittedMouseCapture, true);
  assert.equal(f.mounts.get(surface.id), true);
  assert.equal(f.calls.filter(c => c.method === 'ui/open').length, 2);
  f.runtime.ui.handle('ui/mouse', mouse); assert.equal(d.inputs.length, 1);
  await f.runtime.ui.close(surface); assert.deepEqual(f.errors, []);
});

test('hostile intent churn hits the finite admission bound and releases the last lease without a frame', async () => {
  const f = fixture({ openBarrier: (open, params) => {
    if (open > 1) {
      noFrames(f);
      f.runtime.ui.surfaces.get('pi-1').tui.terminal.write(params.mouse_capture ? disable : enable);
    }
  } }), d = drawing(f);
  await assert.rejects(d.mounting, /bounds_exceeded mouse capture admission changes/);
  assert.equal(f.calls.filter(c => c.method === 'ui/open').length, 9, 'initial geometry plus at most eight re-admissions');
  assert.equal(f.calls.filter(c => c.method === 'ui/close').length, 9);
  noFrames(f); assert.equal(f.mounts.size, 0); assert.equal(f.runtime.ui.surfaces.size, 0);
  assert.equal(d.disposed, 1); assert.deepEqual(f.errors, []);
});

test('constructor done resolves once and never re-admits the disposed component', async () => {
  const f = fixture(), results = [], settled = Promise.withResolvers();
  let disposed = 0;
  const surface = await f.runtime.ui.mount(f.store, 'fullscreen', 'test', (tui, _theme, _keys, done) => {
    tui.terminal.write(enable); done('saved'); done('duplicate');
    return { render: () => ['unused'], dispose() { disposed++; tui.terminal.write(disable); } };
  }, { done: value => { results.push(value); settled.resolve(); } });
  await settled.promise;
  assert.equal(surface.closed, true); assert.equal(disposed, 1); assert.deepEqual(results, ['saved']);
  assert.deepEqual(f.calls.map(c => c.method), ['ui/open', 'ui/close']);
  assert.equal(f.mounts.size, 0); assert.deepEqual(f.errors, []);
});

test('async construction may change intent until completion, without an intermediate frame', async () => {
  const f = fixture(), constructing = Promise.withResolvers(), proceed = Promise.withResolvers();
  let tui;
  const mounting = f.runtime.ui.mount(f.store, 'fullscreen', 'test', async ui => {
    tui = ui; constructing.resolve(); await proceed.promise;
    tui.terminal.write(enable); tui.requestRender();
    return { render: () => ['async canvas'] };
  });
  await constructing.promise; noFrames(f);
  tui.terminal.write(enable); tui.terminal.write(disable); noFrames(f);
  proceed.resolve(); const surface = await mounting;
  assert.equal(surface.requestedMouseCapture, true); assert.equal(surface.admittedMouseCapture, true);
  assert.deepEqual(f.calls.map(c => c.method), ['ui/open', 'ui/close', 'ui/open', 'ui/frame']);
  await f.runtime.ui.close(surface); assert.deepEqual(f.errors, []);
});

test('intent changed during the re-admission close is used for the next open', async () => {
  const f = fixture({ holdClose: true }), d = drawing(f);
  await f.pending; noFrames(f);
  d.tui.terminal.write(disable);
  f.release(); const surface = await d.mounting;
  assert.equal(surface.admittedMouseCapture, false); assert.equal(surface.desiredMouseCapture, false);
  assert.deepEqual(f.calls.filter(c => c.method === 'ui/open').map(c => c.params.mouse_capture), [false, false]);
  await f.runtime.ui.close(surface); assert.deepEqual(f.errors, []);
});

for (const malformedOpen of [1, 2]) test(`malformed geometry on open ${malformedOpen} releases the admitted host mount`, async () => {
  const f = fixture({ malformedOpen }), d = drawing(f);
  await assert.rejects(d.mounting, /ui geometry/);
  noFrames(f); assert.equal(f.mounts.size, 0); assert.equal(f.runtime.ui.surfaces.size, 0);
  assert.equal(d.disposed, malformedOpen === 1 ? 0 : 1); assert.deepEqual(f.errors, []);
});

test('capture declared before mount is admitted once and constructor can relinquish it before painting', async () => {
  const f = fixture();
  f.runtime.scope.run(f.store, () => f.runtime.mouseIntent(true));
  const surface = await f.runtime.ui.mount(f.store, 'fullscreen', 'test', tui => {
    tui.terminal.write(disable); return { render: () => ['without capture'] };
  });
  assert.equal(f.calls[0].params.mouse_capture, true);
  assert.equal(f.calls[2].params.mouse_capture, false);
  assert.equal(f.mounts.get(surface.id), false); assert.equal(surface.admittedMouseCapture, false);
  assert.throws(() => f.runtime.ui.handle('ui/mouse', mouse), /without capture/);
  await f.runtime.ui.close(surface); assert.deepEqual(f.errors, []);
});

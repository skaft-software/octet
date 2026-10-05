import test from 'node:test';
import assert from 'node:assert/strict';
import { exec } from '../lib/exec.mjs';
import { Runtime } from '../lib/runtime.mjs';

function fixture(result = { stdout: 'a b', stderr: 'err', code: 7, killed: false }) {
  const calls = [], notices = [];
  const runtime = {
    require: feature => assert.equal(feature, 'process_exec_v1'), assertOwner() {},
    transport: { childId: 0, notify: async (...args) => { notices.push(args); } },
    hostCall: (method, params, store) => { calls.push({ method, params, store }); ++runtime.transport.childId; return Promise.resolve(result); },
    track: promise => promise, backgroundError: error => { throw error; },
  };
  return { runtime, calls, notices, store: { state: { workspace: '/tmp/work', owner: { session_id: 'owned' } } } };
}
test('exec keeps command, argv, cwd, timeout and native result without shell joining', async () => {
  const f = fixture();
  assert.deepEqual(await exec(f.runtime, f.store, '/bin/echo', ['a b', '$(touch forbidden)'], { cwd: '/tmp/exact', timeout: 123 }),
    { stdout: 'a b', stderr: 'err', code: 7, killed: false });
  assert.deepEqual(f.calls[0].params, { resource_owner: f.store.state.owner, command: '/bin/echo', args: ['a b', '$(touch forbidden)'], cwd: '/tmp/exact', timeout_ms: 123, cancelled: false });
});
test('already aborted signal is transmitted rather than inventing a result', async () => {
  const f = fixture({ stdout: '', stderr: '', code: 0, killed: true });
  const signal = AbortSignal.abort();
  const result = await exec(f.runtime, f.store, '/bin/echo', [], { signal });
  assert.equal(f.calls[0].params.cancelled, true); assert.equal(result.killed, true);
});
test('abort asks native executor to kill but waits for its real partial result', async () => {
  const f = fixture(); const controller = new AbortController(); let settle;
  f.runtime.hostCall = () => { ++f.runtime.transport.childId; return new Promise(resolve => { settle = resolve; }); };
  const pending = exec(f.runtime, f.store, '/bin/sh', [], { signal: controller.signal });
  controller.abort();
  assert.deepEqual(f.notices, [['process/exec/cancel', { id: 'pi:1' }]]);
  settle({ stdout: 'partial', stderr: '', code: 0, killed: true });
  assert.equal((await pending).stdout, 'partial');
});
test('native policy denial is propagated, never returned as success', async () => {
  const f = fixture(); f.runtime.hostCall = () => Promise.reject(new Error('process denied'));
  await assert.rejects(exec(f.runtime, f.store, '/bin/echo', []), /process denied/);
});
test('caught native refusal is not replayed by the real command flush', async () => {
  const f = fixture();
  Object.assign(f.store, { id: 1, live: true, pending: new Set(), errors: [] });
  f.runtime.active = new Map([[f.store.id, f.store]]);
  f.runtime.track = Runtime.prototype.track;
  f.runtime.hostCall = () => Promise.reject(new Error('exact approval was not granted'));
  let caught;
  try { await exec(f.runtime, f.store, '/bin/sh', ['-c', 'touch forbidden']); }
  catch (error) { caught = error; }
  assert.match(caught.message, /exact approval/);
  await Runtime.prototype.flush.call(f.runtime, f.store);
  assert.deepEqual(f.store.errors, []);
});
test('malformed args and unsupported options never reach the host', async () => {
  const f = fixture();
  await assert.rejects(exec(f.runtime, f.store, '/bin/echo', ['bad\0arg']));
  await assert.rejects(exec(f.runtime, f.store, '/bin/echo', [], { env: {} }));
  assert.equal(f.calls.length, 0);
});

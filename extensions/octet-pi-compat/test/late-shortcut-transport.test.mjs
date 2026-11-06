import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { launch, owner } from './helper.mjs';

async function start(t) {
  const dir = await mkdtemp(join(tmpdir(), 'pi-late-shortcut-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const factory = join(dir, 'factory.mjs');
  await writeFile(factory, `export default pi => {
    let invoked = 0;
    pi.registerCommand('verify-shortcut', { handler() {
      if (invoked !== 1) throw Error('late shortcut did not execute exactly once');
    }});
    pi.on('session_start', () => pi.registerShortcut('ctrl+alt+f', {
      description: 'Fleet overlay', handler() { invoked++; }
    }));
  };`);
  const peer = launch(t, [factory], { hold: ['shortcut/register'] });
  const initialized = await peer.init(['shortcuts']);
  assert.deepEqual(initialized.shortcuts, []);
  const hook = peer.request('hook/run', {
    hook: 'session_start', payload: { binding: owner }, context: peer.context({ has_ui: false }),
  });
  const registration = await peer.wait(frame => frame.method === 'shortcut/register');
  assert.deepEqual(registration.params, {
    parent_request_id: hook.id, resource_owner: owner,
    id: 'pi-runtime-shortcut-0', key: 'ctrl+alt+f', description: 'Fleet overlay',
  });
  assert.equal(peer.seen.some(frame => frame.id === hook.id && !frame.method), false);
  return { peer, hook, registration };
}

test('real worker settles native late shortcut registration before accepting its trigger', async t => {
  const { peer, hook, registration } = await start(t);
  peer.send({ jsonrpc: '2.0', id: registration.id, result: {} });
  assert.ok((await hook.response).result);
  peer.notify('shortcut/trigger', { id: registration.params.id, resource_owner: owner });
  assert.ok((await peer.command('verify-shortcut', [], { has_ui: false }).response).result);
  assert.equal(peer.seen.some(frame => frame.method === 'notification' && frame.params.level === 'warning'), false);
  assert.equal(peer.stderr().includes('callback was skipped'), false);
  await peer.close();
});

test('real worker does not install a shortcut after native registration refusal', async t => {
  const { peer, hook, registration } = await start(t);
  peer.send({ jsonrpc: '2.0', id: registration.id, error: { code: -32602, message: 'native shortcut conflict' } });
  assert.ok((await hook.response).result);
  assert.equal(peer.seen.filter(frame => frame.method === 'shortcut/register').length, 1);
  peer.notify('shortcut/trigger', { id: registration.params.id, resource_owner: owner });
  await peer.waitStderr(/unknown shortcut trigger/);
  assert.match((await peer.command('verify-shortcut', [], { has_ui: false }).response).error.message, /late shortcut did not execute/);
  await peer.close();
});

test('real worker treats shortcut registration as effectful during parent cancellation', async t => {
  const { peer, hook, registration } = await start(t);
  peer.notify('$/cancelRequest', { id: hook.id });
  const cancellation = await peer.wait(frame => frame.method === '$/cancelRequest');
  assert.equal(cancellation.params.id, registration.id);
  await sleep(50);
  assert.equal(peer.seen.some(frame => frame.id === hook.id && !frame.method), false);
  peer.send({ jsonrpc: '2.0', id: registration.id, error: { code: -32800, message: 'native shortcut registration cancelled' } });
  assert.equal((await hook.response).error.code, -32800);
  await peer.close();
});

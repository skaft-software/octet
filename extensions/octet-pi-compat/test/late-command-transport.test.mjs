import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { launch, owner } from './helper.mjs';

async function start(t) {
  const dir = await mkdtemp(join(tmpdir(), 'pi-late-command-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const factory = join(dir, 'factory.mjs');
  await writeFile(factory, `export default pi => {
    let invoked = 0;
    pi.registerCommand('verify-late', { description: 'Verify delegate', handler() {
      if (invoked !== 1) throw Error('late handler did not execute exactly once');
    }});
    pi.on('session_start', () => pi.registerCommand('mcp:start', {
      description: 'Start delegates', handler(args) {
        if (args !== 'worker') throw Error('wrong delegate arguments');
        invoked++;
      }
    }));
  };`);
  const peer = launch(t, [factory]);
  const initialized = await peer.init(['runtime_commands', 'runtime_command_registration_v1']);
  assert.deepEqual(initialized.commands.map(command => command.name), ['verify-late']);
  const hook = peer.request('hook/run', {
    hook: 'session_start', payload: { binding: owner }, context: peer.context({ has_ui: false }),
  });
  const registration = await peer.wait(frame => frame.method === 'commands/register');
  assert.deepEqual(registration.params, {
    parent_request_id: hook.id, resource_owner: owner,
    commands: [{ name: 'mcp:start', description: 'Start delegates' }],
  });
  assert.equal(peer.seen.some(frame => frame.id === hook.id && !frame.method), false);
  return { peer, hook, registration };
}

test('real worker publishes a session_start command before executing its live handler', async t => {
  const { peer, hook, registration } = await start(t);
  peer.send({ jsonrpc: '2.0', id: registration.id, result: { commands: ['verify-late', 'mcp:start'] } });
  assert.ok((await hook.response).result);
  assert.ok((await peer.command('mcp:start', ['worker'], { has_ui: false }).response).result);
  assert.ok((await peer.command('verify-late', [], { has_ui: false }).response).result);
  assert.equal(peer.seen.some(frame => frame.method === 'notification' && frame.params.level === 'warning'), false);
  assert.equal(peer.stderr().includes('callback was skipped'), false);
  await peer.close();
});

test('real worker preserves native registration refusal without publishing the local handler', async t => {
  const { peer, hook, registration } = await start(t);
  peer.send({ jsonrpc: '2.0', id: registration.id, error: { code: -32602, message: 'native registration refused' } });
  assert.ok((await hook.response).result);
  const response = await peer.command('mcp:start', [], { has_ui: false }).response;
  assert.match(response.error.message, /unknown command/);
  assert.equal(peer.seen.filter(frame => frame.method === 'commands/register').length, 1);
  await peer.close();
});

test('real worker treats cancelled command registration as effectful and awaits native settlement', async t => {
  const { peer, hook, registration } = await start(t);
  peer.notify('$/cancelRequest', { id: hook.id });
  const cancellation = await peer.wait(frame => frame.method === '$/cancelRequest');
  assert.equal(cancellation.params.id, registration.id);
  await sleep(50);
  assert.equal(peer.seen.some(frame => frame.id === hook.id && !frame.method), false);
  peer.send({ jsonrpc: '2.0', id: registration.id, error: { code: -32800, message: 'native registration cancelled' } });
  assert.equal((await hook.response).error.code, -32800);
  const response = await peer.command('mcp:start', [], { has_ui: false }).response;
  assert.match(response.error.message, /unknown command/);
  await peer.close();
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';

const factory = join(root, 'test/fixtures/agent-settled.ts');

test('agent_settled loads and fires once after agent_end when the owning run settles', async t => {
  const peer = launch(t, [factory]); await peer.init(['lifecycle_events', 'session_entries']); await peer.start();
  peer.notify('turn/started', { resource_owner: owner });
  peer.notify('turn/settled', { resource_owner: owner, outcome: 'completed' });
  await peer.wait(frame => frame.method === 'notification' && frame.params.message.startsWith('run:settled:'));
  const runs = peer.seen.filter(frame => frame.method === 'notification' && frame.params.message.startsWith('run:')).map(frame => frame.params.message);
  assert.deepEqual(runs, ['run:end', 'run:settled:{"type":"agent_settled","aborted":false}']);
  await peer.close();
});

test('agent_settled reports a cancelled run as aborted, as Pi 1.1 does', async t => {
  const peer = launch(t, [factory]); await peer.init(['lifecycle_events', 'session_entries']); await peer.start();
  peer.notify('turn/started', { resource_owner: owner });
  peer.notify('turn/settled', { resource_owner: owner, outcome: 'cancelled' });
  await peer.wait(frame => frame.method === 'notification' && frame.params.message.startsWith('run:settled:'));
  const settled = peer.seen.find(frame => frame.method === 'notification' && frame.params.message.startsWith('run:settled:'));
  assert.equal(settled.params.message, 'run:settled:{"type":"agent_settled","aborted":true}');
  await peer.close();
});

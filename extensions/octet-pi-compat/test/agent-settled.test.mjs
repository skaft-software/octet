import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';

const factory = join(root, 'test/fixtures/agent-settled.ts');

test('agent_settled loads and fires once after agent_end when the owning run settles', async t => {
  const peer = launch(t, [factory]); await peer.init(['lifecycle_events']); await peer.start();
  peer.notify('turn/started', { resource_owner: owner });
  peer.notify('turn/settled', { resource_owner: owner, outcome: 'completed' });
  await peer.wait(frame => frame.method === 'notification' && frame.params.message.startsWith('run:settled:'));
  const runs = peer.seen.filter(frame => frame.method === 'notification' && frame.params.message.startsWith('run:')).map(frame => frame.params.message);
  assert.deepEqual(runs, ['run:end', 'run:settled:{"type":"agent_settled"}']);
  await peer.close();
});

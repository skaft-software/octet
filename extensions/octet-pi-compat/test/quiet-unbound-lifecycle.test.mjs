import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';

const factory = join(root, 'test/fixtures/standalone-observation.ts');
const issues = peer => peer.seen.filter(frame => frame.method === 'notification'
  && frame.params?.title === 'Pi compatibility error');
const lifecycle = ['turn/started', 'message/started', 'message/settled', 'tool/started', 'tool/settled', 'turn/settled'];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

// An empty bridge subscribes to no hook, so the host never sends it session_start
// and the adapter never binds an owner. The host still sends lifecycle
// notifications for every prompt; refusing them must not surface as an error.
test('lifecycle notifications for an owner the adapter never bound raise no user-visible error', async t => {
  const peer = launch(t, []);
  await peer.init(['session_entries', 'lifecycle_events', 'lifecycle_events_v2']);
  for (const method of lifecycle) peer.notify(method, { resource_owner: owner, message_id: 'm1', tool_call_id: 't1' });
  await pause(300);
  assert.deepEqual(issues(peer), []);
  await peer.close();
});

test('a retired or foreign owner is also refused without an error, and bound owners still receive events', async t => {
  const peer = launch(t, [factory]);
  await peer.init(['session_entries', 'lifecycle_events', 'lifecycle_events_v2']);
  await peer.start();
  const foreign = { ...owner, session_id: 'never-bound-session' };
  for (const method of lifecycle) peer.notify(method, { resource_owner: foreign, message_id: 'm1', tool_call_id: 't1' });
  await pause(300);
  assert.deepEqual(issues(peer), []);
  // The bound owner keeps working: its settled turn reaches the factory.
  peer.notify('turn/started', { resource_owner: owner });
  peer.notify('turn/settled', { resource_owner: owner });
  const frame = await peer.wait(f => f.method === 'notification' && f.params.message?.startsWith('standalone:'));
  assert.equal(JSON.parse(frame.params.message.slice('standalone:'.length)).type, 'agent_end');
  assert.deepEqual(issues(peer), []);
  await peer.close();
});

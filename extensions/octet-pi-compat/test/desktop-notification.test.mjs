import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, owner, root } from './helper.mjs';
const fixture = join(root, 'test/fixtures/desktop-notification.ts');

test('OSC777 becomes an owner-fenced host notification intent, never raw protocol stdout', async t => {
  const peer = launch(t, [fixture], { hold: ['ui/chrome'] }); await peer.init();
  const request = peer.command('desktop-notify');
  const call = await Promise.race([
    peer.wait(f => f.method === 'ui/chrome'),
    request.response.then(frame => { assert.fail(JSON.stringify(frame)); }),
  ]);
  assert.deepEqual(call.params, { parent_request_id: request.id, resource_owner: owner,
    chrome: { kind: 'desktop_notification', title: 'π', body: 'Hello from the mock provider.' } });
  assert.equal(peer.seen.some(f => f.id === request.id && !f.method), false);
  peer.send({ jsonrpc: '2.0', id: call.id, result: { tools_expanded: false } });
  assert.ok((await request.response).result);
  await peer.close();
});

test('desktop notification refuses absent UI, injected escapes, oversize text and host refusal', async t => {
  const noUI = launch(t, [fixture]); await noUI.init([]);
  assert.match((await noUI.command('desktop-notify').response).error.message, /unsupported_feature remote_ui/);
  await noUI.close();
  const peer = launch(t, [fixture], { hold: ['ui/chrome'] }); await peer.init();
  assert.match((await peer.command('desktop-notify', ['unsafe']).response).error.message, /direct terminal control/);
  assert.match((await peer.command('desktop-notify', ['oversize']).response).error.message, /bounds_exceeded/);
  const request = peer.command('desktop-notify');
  const call = await peer.wait(f => f.method === 'ui/chrome');
  peer.send({ jsonrpc: '2.0', id: call.id, error: { code: -32002, message: 'not_foreground_owner' } });
  assert.match((await request.response).error.message, /not_foreground_owner/);
  await peer.close();
});

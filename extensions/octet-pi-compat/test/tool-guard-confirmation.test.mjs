import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, root } from './helper.mjs';

const fixture = join(root, 'test/fixtures/asking-guard.ts');

for (const [name, keys, expected] of [
  ['default No', ['Enter'], 'deny'],
  ['explicit Yes', ['ArrowDown', 'Enter'], 'continue'],
  ['Escape', ['Escape'], 'deny'],
]) test(`tool guard confirmation: ${name}`, async t => {
  const peer = launch(t, [fixture]);
  await peer.init(['remote_ui']); await peer.start({ has_ui: true });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'bash', tool_call_id: 'call-1', arguments: { command: 'true' } },
    context: peer.context({ has_ui: true }),
  });
  const opened = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'fullscreen');
  const id = opened.params.surface_id;
  const frame = await peer.wait(f => f.method === 'ui/frame' && f.params.surface_id === id);
  assert.match(frame.params.lines.join('\n'), /Allow\?/);
  assert.match(frame.params.lines.join('\n'), /run bash/);
  for (const key of keys) peer.notify('ui/key', { surface_id: id, key, kind: 'press', modifiers: [] });
  const response = await request.response;
  assert.equal(response.result?.disposition.action, expected, JSON.stringify(response));
  assert.equal(peer.seen.some(f => f.method === 'confirmation/request'), false);
  await peer.close();
});

test('tool guard confirmation without UI is not approved', async t => {
  const peer = launch(t, [fixture], { hold: ['confirmation/request'] });
  await peer.init(['remote_ui']); await peer.start({ has_ui: false });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'bash', tool_call_id: 'call-1', arguments: { command: 'true' } },
    context: peer.context({ has_ui: false }),
  });
  const response = await request.response;
  assert.equal(response.result?.disposition.action, 'deny', JSON.stringify(response));
  assert.equal(peer.seen.some(f => f.method === 'confirmation/request'), false);
  assert.equal(peer.seen.some(f => f.method === 'ui/open'), false);
  await peer.close();
});

test('retiring the confirmation surface denies the guard', async t => {
  const peer = launch(t, [fixture]);
  await peer.init(['remote_ui']); await peer.start({ has_ui: true });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'bash', tool_call_id: 'call-1', arguments: { command: 'true' } },
    context: peer.context({ has_ui: true }),
  });
  const opened = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'fullscreen');
  await peer.wait(f => f.method === 'ui/frame' && f.params.surface_id === opened.params.surface_id);
  peer.notify('ui/closed', { surface_id: opened.params.surface_id, reason: 'host rescue' });
  const response = await request.response;
  assert.equal(response.result?.disposition.action, 'deny', JSON.stringify(response));
  await peer.close();
});

test('cancelling a waiting tool guard cannot approve it', async t => {
  const peer = launch(t, [fixture]);
  await peer.init(['remote_ui']); await peer.start({ has_ui: true });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'bash', tool_call_id: 'call-1', arguments: { command: 'true' } },
    context: peer.context({ has_ui: true }),
  });
  const opened = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'fullscreen');
  await peer.wait(f => f.method === 'ui/frame' && f.params.surface_id === opened.params.surface_id);
  peer.notify('$/cancelRequest', { id: request.id });
  const response = await request.response;
  assert.ok(response.error || response.result?.disposition.action === 'deny', JSON.stringify(response));
  await peer.close();
});

test('foreground guard question retains the bounded user-wait allowance', async t => {
  const peer = launch(t, [fixture], { env: { OCTET_PI_HANDLER_BUDGET_MS: '100' } });
  await peer.init(['remote_ui']); await peer.start({ has_ui: true });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'bash', tool_call_id: 'call-1', arguments: { command: 'true' } },
    context: peer.context({ has_ui: true }),
  });
  const opened = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'fullscreen');
  const id = opened.params.surface_id;
  await peer.wait(f => f.method === 'ui/frame' && f.params.surface_id === id);
  await new Promise(resolve => setTimeout(resolve, 500));
  peer.notify('ui/key', { surface_id: id, key: 'ArrowDown', kind: 'press', modifiers: [] });
  peer.notify('ui/key', { surface_id: id, key: 'Enter', kind: 'press', modifiers: [] });
  const response = await request.response;
  assert.equal(response.result?.disposition.action, 'continue', JSON.stringify(response));
  await peer.close();
});

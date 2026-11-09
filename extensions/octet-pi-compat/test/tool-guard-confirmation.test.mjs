import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, root } from './helper.mjs';

const fixture = join(root, 'test/fixtures/asking-guard.ts');

// Model the host's two-phase guard/dispatch with an observable benign tool.
async function dispatchAfterGuard(peer, response, expectedExecutions) {
  if (response.result?.disposition.action === 'continue') {
    const tool = await peer.request('tool/call', { name: 'guard_probe', arguments: {}, context: peer.context() }).response;
    assert.equal(tool.result?.content[0].text, 'executed');
  }
  assert.ok((await peer.command('guard-count').response).result);
  await peer.wait(f => f.method === 'notification' && f.params.message === 'guard-executions:' + expectedExecutions);
  if (expectedExecutions === 0) assert.doesNotMatch(peer.stderr(), /guarded tool executed/);
}

for (const [name, keys, expected] of [
  ['default No', ['Enter'], 'deny'],
  ['explicit Yes', ['ArrowDown', 'Enter'], 'continue'],
  ['Escape', ['Escape'], 'deny'],
]) test(`tool guard confirmation: ${name}`, async t => {
  const peer = launch(t, [fixture]);
  await peer.init(['remote_ui']); await peer.start({ has_ui: true });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'guard_probe', tool_call_id: 'call-1', arguments: { command: 'true' } },
    context: peer.context({ has_ui: true }),
  });
  const opened = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'fullscreen');
  const id = opened.params.surface_id;
  const frame = await peer.wait(f => f.method === 'ui/frame' && f.params.surface_id === id);
  assert.match(frame.params.lines.join('\n'), /Allow\?/);
  assert.match(frame.params.lines.join('\n'), /run guard_probe/);
  for (const key of keys) peer.notify('ui/key', { surface_id: id, key, kind: 'press', modifiers: [] });
  const response = await request.response;
  assert.equal(response.result?.disposition.action, expected, JSON.stringify(response));
  assert.equal(peer.seen.some(f => f.method === 'confirmation/request'), false);
  await dispatchAfterGuard(peer, response, expected === 'continue' ? 1 : 0);
  await peer.close();
});

for (const features of [['remote_ui'], []]) test('tool guard confirmation without UI is not approved' + (features.length ? '' : ' (unnegotiated)'), async t => {
  const peer = launch(t, [fixture]);
  await peer.init(features); await peer.start({ has_ui: false });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'guard_probe', tool_call_id: 'call-1', arguments: { command: 'true' } },
    context: peer.context({ has_ui: false }),
  });
  // Pi returns false immediately without UI, not a question for an absent
  // user: pi-reference/node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/runner.js:132-135,957-967.
  const response = await request.response;
  assert.equal(response.result?.disposition.action, 'deny', JSON.stringify(response));
  assert.equal(response.result.disposition.reason, 'user said no');
  await dispatchAfterGuard(peer, response, 0);
  assert.equal(peer.seen.some(f => ['ui/open', 'ui/frame', 'confirmation/request'].includes(f.method)), false);
  assert.equal(peer.seen.filter(f => f.id === request.id && !f.method).length, 1);
  await peer.close();
});

test('retiring the confirmation surface denies the guard', async t => {
  const peer = launch(t, [fixture]);
  await peer.init(['remote_ui']); await peer.start({ has_ui: true });
  const request = peer.request('hook/run', {
    hook: 'before_tool_call',
    payload: { name: 'guard_probe', tool_call_id: 'call-1', arguments: { command: 'true' } },
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
    payload: { name: 'guard_probe', tool_call_id: 'call-1', arguments: { command: 'true' } },
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
    payload: { name: 'guard_probe', tool_call_id: 'call-1', arguments: { command: 'true' } },
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

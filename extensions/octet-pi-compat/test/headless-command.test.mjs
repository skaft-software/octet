import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

function commands(t) {
  const directory = mkdtempSync(join(tmpdir(), 'pi-headless-command-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.mjs');
  writeFileSync(entry, `export default pi => {
    pi.registerCommand('plain', {handler: (_args, ctx) => ctx.ui.notify('hasUI:' + ctx.hasUI)});
    pi.registerCommand('draft', {handler: (_args, ctx) => ctx.ui.notify('draft:' + ctx.ui.getEditorText())});
    pi.registerCommand('edit', {handler: (_args, ctx) => ctx.ui.setEditorText('must not be accepted')});
  };`);
  // Do not let the protocol peer auto-ACK composer reads. Headless cases
  // refuse them; UI cases explicitly supply changing host snapshots.
  return launch(t, [entry], { hold: ['composer/get', 'composer/set'] });
}

function refuseComposer(peer, request) {
  peer.send({ jsonrpc: '2.0', id: request.id, error: {
    code: -32602, message: 'invalid_request: no foreground composer is available in this host mode',
  } });
}

for (const features of [['composer'], ['composer', 'remote_ui']]) {
  test(`headless commands do not prefetch a composer (${features.join(', ')})`, async t => {
    const peer = commands(t);
    await peer.init(features);
    const command = peer.command('plain', [], { has_ui: false });
    const first = await peer.wait(frame => ['composer/get', 'notification'].includes(frame.method));
    if (first.method === 'composer/get') refuseComposer(peer, first);
    const response = await command.response;
    assert.ok(response.result, JSON.stringify(response));
    assert.equal(first.method, 'notification');
    assert.equal(first.params.message, 'hasUI:false');
    assert.ok(!peer.seen.some(frame => frame.method === 'composer/get'));
    await peer.close();
  });
}

test('UI commands refresh the actual host composer before each handler', async t => {
  const peer = commands(t);
  await peer.init(['composer', 'remote_ui']);
  for (const text of ['first host draft', 'changed host draft']) {
    const command = peer.command('draft', [], { has_ui: true });
    const get = await peer.wait(frame => frame.method === 'composer/get');
    assert.equal(get.params.parent_request_id, command.id);
    peer.send({ jsonrpc: '2.0', id: get.id, result: { text } });
    const response = await command.response;
    assert.ok(response.result, JSON.stringify(response));
    const notice = await peer.wait(frame => frame.method === 'notification');
    assert.equal(notice.params.message, `draft:${text}`);
  }
  assert.equal(peer.seen.filter(frame => frame.method === 'composer/get').length, 2);
  await peer.close();
});

test('explicit headless composer mutations still propagate host refusals', async t => {
  const peer = commands(t);
  await peer.init(['composer', 'remote_ui']);
  const command = peer.command('edit', [], { has_ui: false });
  const request = await peer.wait(frame => ['composer/get', 'composer/set'].includes(frame.method));
  refuseComposer(peer, request);
  const response = await command.response;
  assert.equal(request.method, 'composer/set');
  assert.equal(request.params.parent_request_id, command.id);
  assert.equal(response.error.code, -32602);
  assert.match(response.error.message, /no foreground composer/);
  assert.ok(!peer.seen.some(frame => frame.method === 'composer/get'));
  await peer.close();
});

test('UI command snapshot failures are not swallowed to execute a stale handler', async t => {
  const peer = commands(t);
  await peer.init(['composer', 'remote_ui']);
  const command = peer.command('draft', [], { has_ui: true });
  const request = await peer.wait(frame => frame.method === 'composer/get');
  refuseComposer(peer, request);
  const response = await command.response;
  assert.equal(response.error.code, -32602);
  assert.match(response.error.message, /no foreground composer/);
  assert.ok(!peer.seen.some(frame => frame.method === 'notification'));
  await peer.close();
});

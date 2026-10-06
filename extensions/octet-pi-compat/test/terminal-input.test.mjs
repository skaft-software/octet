import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, root, owner } from './helper.mjs';

async function started(t, mode, features = ['remote_ui', 'terminal_input_intercept_v1']) {
  const peer = launch(t, [join(root, 'test/fixtures/terminal-input.ts')]);
  await peer.init(features);
  const reply = await peer.command('listen', [mode]).response;
  return {peer, reply};
}
const input = (peer, data, resource_owner = owner) => peer.request('ui/terminal-input/intercept', {data, resource_owner}).response;
async function trace(peer) {
  const reply = await peer.request('tool/call', {name:'terminal_state', arguments:{}, context:peer.context()}).response;
  assert.ok(reply.result, JSON.stringify(reply));
  return JSON.parse(reply.result.content[0].text);
}

test('Pi pre-native chain consumes, transforms in registration order, and passes raw strings', async t => {
  const {peer, reply} = await started(t, 'chain'); assert.ok(reply.result, JSON.stringify(reply));
  assert.deepEqual((await input(peer, 'x')).result, {data:''});
  assert.deepEqual((await input(peer, 'a')).result, {data:'Z'});
  for (const data of ['q', '\x1bOA', '\x1b[97;5:3u', '\x1b[200~雪\nx\x1b[201~']) {
    assert.deepEqual((await input(peer, data)).result, {data});
  }
  assert.deepEqual((await trace(peer)).slice(0, 3), [['first','x'], ['first','a'], ['second','b']]);
  await peer.close();
});

test('Pi live Set skips removed handlers, deduplicates identity and runs newly added handlers', async t => {
  const {peer} = await started(t, 'set');
  assert.deepEqual((await input(peer, 'a')).result, {data:'L'});
  assert.deepEqual(await trace(peer), [['first','a'], ['later','a']]);
  await peer.close();
});

test('empty transforms reach later listeners; promises are not awaited or treated as consumes', async t => {
  for (const mode of ['empty', 'promise']) {
    const {peer} = await started(t, mode);
    assert.deepEqual((await input(peer, 'a')).result, {data:mode === 'empty' ? '' : 'a'});
    if (mode === 'empty') assert.deepEqual(await trace(peer), [['empty','']]);
    await peer.close();
  }
});

test('unsubscribe and owner retirement remove listeners; foreign owners never receive input', async t => {
  const {peer} = await started(t, 'ordinary');
  assert.deepEqual((await input(peer, 'a')).result, {data:'[a]'});
  assert.ok((await peer.command('listen', ['remove']).response).result);
  assert.deepEqual((await input(peer, 'a')).result, {data:'a'});
  assert.ok((await peer.command('listen', ['ordinary']).response).result);
  const next = {...owner, session_id:'next-owner'};
  assert.ok((await input(peer, 'a', next)).error);
  const context = {...peer.context(), resource_owner:next};
  assert.ok((await peer.request('hook/run', {hook:'session_start', payload:{binding:next}, context}).response).result);
  assert.deepEqual((await input(peer, 'a', next)).result, {data:'a'});
  assert.ok((await input(peer, 'a')).error);
  await peer.close();
});

test('interception requires negotiation and keeps the native 256-byte bound', async t => {
  const missing = await started(t, 'chain', ['remote_ui']);
  assert.match(missing.reply.error.message, /terminal_input_intercept_v1/);
  await missing.peer.close();
  const {peer} = await started(t, 'oversize');
  assert.match((await input(peer, 'a')).error.message, /bounds_exceeded/);
  assert.match((await input(peer, 'x'.repeat(257))).error.message, /bounds_exceeded/);
  await peer.close();
});

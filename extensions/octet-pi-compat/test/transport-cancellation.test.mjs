// Recorded matrix repro (pi-mcp-adapter, @plannotator/pi-extension): the host
// drops a synchronous reverse request together with the hook that issued it and
// later delivers its reply. The adapter must treat that as terminal
// cancellation and keep serving later hooks instead of losing the extension
// process, which the host reports as "extension stdout closed".
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';
import { Transport } from '../lib/transport.mjs';

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'pi-transport-cancel-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.mjs');
  writeFileSync(entry, `export default pi => {
    // The real packages' synchronous catalog read inside a hook:
    // pi.getActiveTools() -> tools/snapshot.
    pi.on('session_start', () => { pi.getActiveTools(); });
    pi.on('before_agent_start', () => { pi.getActiveTools(); });
    pi.registerCommand('probe', { handler: () => { pi.getActiveTools(); } });
  };`);
  return entry;
}

// One hook issues the synchronous request; the host drops it and replies late.
async function droppedSyncRequest(t, drop) {
  const peer = launch(t, [fixture(t)]);
  await peer.init(['active_tools', 'before_prompt_state_v1']);
  const start = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() }).response;
  const snapshot = await peer.wait(frame => frame.method === 'tools/snapshot');
  assert.match(String(snapshot.id), /^pi:/);
  drop(peer, snapshot);
  await start;
  assert.equal(peer.child.exitCode, null, `extension exited: ${peer.stderr()}`);
  return peer;
}

// Answer the catalog read the fixture's command issues, then require its result.
async function probe(peer) {
  const command = peer.command('probe');
  const snapshot = await peer.wait(frame => frame.method === 'tools/snapshot' && frame.params.parent_request_id === command.id);
  peer.send({ jsonrpc: '2.0', id: snapshot.id, result: { active_tools: ['core'], all_tools: ['core'] } });
  const response = await command.response;
  assert.ok(response.result, JSON.stringify(response));
  assert.equal(peer.child.exitCode, null, `extension exited: ${peer.stderr()}`);
}

test('a dropped synchronous request is cancelled, and its late reply does not kill the extension', async t => {
  const peer = await droppedSyncRequest(t, (p, snapshot) => {
    p.notify('$/cancelRequest', { id: snapshot.id, reason: 'request dropped' });
    p.notify('$/cancelRequest', { id: 2, reason: 'request dropped' });
    p.send({ jsonrpc: '2.0', id: snapshot.id, error: { code: -32800, message: 'request dropped' } });
  });
  await probe(peer);
  await peer.close();
});

test('cancelling only the issuing hook still survives the request reply', async t => {
  const peer = await droppedSyncRequest(t, (p, snapshot) => {
    // The parent hook is dropped first; the request reply then arrives without
    // any cancellation for it. Both orders are on the recorded wire.
    p.notify('$/cancelRequest', { id: 2, reason: 'request dropped' });
    p.send({ jsonrpc: '2.0', id: snapshot.id, error: { code: -32800, message: 'request dropped' } });
  });
  await probe(peer);
  await peer.close();
});

test('close is bounded when a worker cannot be terminated (Bun pending fd read)', async () => {
  // Bun reads the inherited stdin pipe with fs.read, and worker.terminate()
  // cannot resolve while that read is outstanding. Shutdown must not wait.
  const transport = new Transport({ write() {}, output: {} }, { onMessage() {}, onLost() {} });
  transport.worker = { terminate: () => new Promise(() => {}) };
  const started = Date.now();
  await transport.close();
  assert.equal(transport.closed, true);
  assert.ok(Date.now() - started < 2000, 'close waited on an unstoppable worker');
});

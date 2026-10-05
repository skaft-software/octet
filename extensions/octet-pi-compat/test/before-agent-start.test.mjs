import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { launch, root } from './helper.mjs';
import { configure } from '../configure.mjs';
const fixture = join(root, 'test/fixtures/before-agent-start.ts');
const params = (peer, prompt, extra) => ({ hook: 'before_prompt', payload: { prompt, system_prompt: 'real base' }, context: peer.context(extra) });
test('before_agent_start maps actual base and ordered replacements, including empty replacement', async t => {
  const peer = launch(t, [fixture]); await peer.init(['before_prompt_state_v1']);
  assert.deepEqual((await peer.request('hook/run', params(peer, 'chain')).response).result, { disposition: { action: 'continue' }, context: [], notifications: [], system_prompt: 'real base\nfirst\nsecond' });
  assert.equal((await peer.request('hook/run', params(peer, 'empty')).response).result.system_prompt, '');
  assert.equal(Object.hasOwn((await peer.request('hook/run', params(peer, 'unchanged')).response).result, 'system_prompt'), false);
  const custom = await peer.request('hook/run', params(peer, 'unsupported')).response;
  assert.ok(!custom.error, JSON.stringify(custom.error));
  assert.deepEqual(custom.result.custom_messages, [{ custom_type: 'not-bound', content: 'not dropped', display: false }]);
  assert.ok((await peer.request('hook/run', params(peer, 'nul')).response).error);
  await peer.close();
});
test('before_agent_start requires native feature and configure records actual system_prompt permission', async t => {
  const peer = launch(t, [fixture]); await assert.rejects(peer.init([]), /before_prompt_state_v1/);
  const dir = mkdtempSync(join(tmpdir(), 'pi-before-prompt-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const output = join(dir, 'octet-pi-compat'); configure({ reviewed: true, output, extensions: [fixture] });
  assert.match(readFileSync(join(output, 'extension.toml'), 'utf8'), /system_prompt = true/);
});
test('cancelled before_agent_start cannot publish a late effective system', async t => {
  const peer = launch(t, [fixture], { hold: ['confirmation/request'] }); await peer.init(['before_prompt_state_v1', 'remote_ui']);
  const request = peer.request('hook/run', params(peer, 'chain', { session_name: 'wait' }));
  const pending = await peer.wait(frame => frame.method === 'confirmation/request'); peer.notify('$/cancelRequest', { id: request.id });
  assert.equal((await request.response).error.code, -32800);
  peer.send({ jsonrpc: '2.0', id: pending.id, result: { confirmed: true } }); await peer.close();
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, existsSync, rmSync, unlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

const view = (provider, id) => ({ provider, id, api: 'openai-completions', name: id, reasoning: false, input: ['text'], context_window: 8192, max_tokens: 1024, cost: { input: 0, output: 0, cache_read: 0, cache_write: 0 } });

test('real catalog command distinguishes an unserved composer read from registry publication', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'pi-catalog-command-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'inventory.mjs'), trace = join(directory, 'inventory.json');
  writeFileSync(entry, `import { writeFileSync } from 'node:fs';
export default pi => {
  pi.registerProvider('catalog-extension', {
    baseUrl: 'http://127.0.0.1:9/', api: 'openai-completions', apiKey: 'extension-catalog-secret',
    models: [{ id: 'extension-model', name: 'Extension model', reasoning: false,
      input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 8192, maxTokens: 1024 }],
    streamSimple: async function* () { throw new Error('inventory must not infer'); },
  });
  pi.registerCommand('inventory', { handler: (_, ctx) => {
    writeFileSync(${JSON.stringify(trace)}, JSON.stringify({ all: ctx.modelRegistry.getAll(), available: ctx.modelRegistry.getAvailable(),
      found: ctx.modelRegistry.find('fixture', 'unconfigured-model'), extension: ctx.modelRegistry.find('catalog-extension', 'extension-model'),
      missing: ctx.modelRegistry.find('fixture', 'missing') === undefined,
      configured: ctx.modelRegistry.hasConfiguredAuth({ provider: 'fixture' }),
      unknownAuth: ctx.modelRegistry.hasConfiguredAuth({ provider: 'missing' }),
      extensionAuth: ctx.modelRegistry.hasConfiguredAuth({ provider: 'catalog-extension' }) }));
  }});
};`);
  const peer = launch(t, [entry], { hold: ['composer/get'] });
  await peer.init(['composer', 'provider_proxy_v1']);
  const registration = await peer.wait(frame => frame.method === 'providers/register');
  peer.send({ jsonrpc: '2.0', id: registration.id, result: { revision: 1 } });
  await peer.wait(frame => frame.method === 'providers/complete');
  const configured = view('fixture', 'test-model'), unconfigured = view('fixture', 'unconfigured-model'), extension = view('catalog-extension', 'extension-model');
  const facts = { pi_models: { all_models_status: { status: 'catalog' }, all_models: [configured, unconfigured, extension], available_models: [configured, extension], configured_auth: { fixture: true, 'catalog-extension': true } } };
  const missingMode = peer.command('inventory', [], facts);
  const blocked = await peer.wait(frame => frame.method === 'composer/get');
  assert.equal(blocked.params.parent_request_id, missingMode.id);
  assert.equal(existsSync(trace), false);
  peer.send({ jsonrpc: '2.0', id: blocked.id, result: { text: '' } });
  assert.ok((await missingMode.response).result);
  const first = JSON.parse(readFileSync(trace, 'utf8'));
  unlinkSync(trace);
  const composerReads = peer.seen.filter(frame => frame.method === 'composer/get').length;
  assert.ok((await peer.command('inventory', [], { ...facts, has_ui: false }).response).result);
  assert.equal(peer.seen.filter(frame => frame.method === 'composer/get').length, composerReads);
  const snapshot = JSON.parse(readFileSync(trace, 'utf8'));
  assert.deepEqual(snapshot, first);
  assert.equal(snapshot.all.length, 3);
  assert.equal(snapshot.available.length, 2);
  assert.equal(snapshot.found.id, 'unconfigured-model');
  assert.equal(snapshot.extension.id, 'extension-model');
  assert.equal(snapshot.missing, true);
  assert.equal(snapshot.configured, true);
  assert.equal(snapshot.unknownAuth, false);
  assert.equal(snapshot.extensionAuth, true);
  assert.doesNotMatch(JSON.stringify(snapshot), /extension-catalog-secret|127\.0\.0\.1/);
  await peer.close();
});

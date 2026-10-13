import test from 'node:test';
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { launch, root, host } from './helper.mjs';

// Footers such as powerline-footer read these two facts inside one try block, so
// a throw or an undefined from either one silently drops the context and cost
// segments they would otherwise render.
const fixture = join(root, 'test/fixtures/footer-facts.ts');
async function facts(t, extra) {
  const peer = launch(t, [fixture]); await peer.init(); await peer.start(extra);
  const result = await peer.request('tool/call', { name: 'facts', arguments: {}, context: peer.context(extra) }).response;
  assert.ok(result.result, JSON.stringify(result));
  const value = JSON.parse(result.result.content[0].text);
  await peer.close();
  return value;
}

test('an unmeasured conversation still reports the model capacity with unknown usage', async t => {
  assert.deepEqual((await facts(t, { context_usage: null })).usage, { tokens: null, contextWindow: 32768, percent: null });
});

test('a measured usage, including a measured zero, passes through unchanged', async t => {
  const usage = { tokens: 0, contextWindow: 272000, percent: 0 };
  assert.deepEqual((await facts(t, { context_usage: usage })).usage, usage);
});

test('no model capacity means no usage at all', async t => {
  assert.equal((await facts(t, { context_usage: null, model_view: null, model: null })).usage, null);
});

test('isUsingOAuth reports the host-stated subscription login and refuses what the host did not state', async t => {
  assert.equal((await facts(t, { model_view: { ...host.model_view, oauth: true } })).oauth, true);
  assert.equal((await facts(t, { model_view: { ...host.model_view, oauth: false } })).oauth, false);
  // An unclassified resolver is refused, never guessed from the endpoint.
  assert.equal((await facts(t, {})).oauth, 'refused');
});

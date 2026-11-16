// Self-contained boundary reproduction, not original-package qualification or
// a footer workaround. Pi's numeric usage.cost contract is still an open gap:
// a shared context/cost catch can retain stale values when native cost is null.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner, host } from './helper.mjs';

const plain = frame => frame.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '');
const namedFrame = name => frame => frame.method === 'ui/frame' && plain(frame).includes(`phase:${name}\n`);
const usage = { input_tokens: 120000, output_tokens: 3810, cache_read_tokens: 0, cache_write_tokens: 0,
  cache_write_1h_tokens: 0, reasoning_tokens: 0, total_tokens: 123810 };
const cost = { input: 100000, output: 25000, reasoning: 0, cache_read: 0, cache_write: 0,
  total: 125000, total_picodollars_remainder: 0 };
const assistant = (id, parent, recordedCost, content = [{ Text: 'answer' }]) => ({
  id, parent, timestamp_unix_ms: 1000,
  value: { type: 'message', Assistant: { model: 'test-model', protocol: 'open_ai_chat', content } },
  ...(recordedCost === undefined ? {} : { assistant_metadata: { assistant_entry_id: id, model: 'test-model', usage, cost: recordedCost, stop_reason: 'end_turn' } }),
});

// Deliberately tiny synthetic consumer of public Pi APIs. No installed factory
// is discovered, copied or loaded. Its combined refresh has the failure shape
// reported for the powerline footer; the refused column makes the cause visible.
const factory = `export default pi => pi.on('session_start', (_, ctx) => {
  let entries = { tokens: 0, total: 0, refused: 0 }, branch = { ...entries };
  function refresh(read, previous) {
    try {
      const tokens = ctx.getContextUsage().tokens;
      let total = 0;
      for (const entry of read()) if (entry.type === 'message' && entry.message.role === 'assistant') {
        total += entry.message.usage?.cost?.total ?? 0;
      }
      return { tokens, total, refused: 0 };
    } catch (error) { return { ...previous, refused: error.code }; }
  }
  const line = (name, value) => name + ':tokens=' + value.tokens + ' total=' + value.total.toFixed(3) + ' refused=' + value.refused;
  ctx.ui.setFooter(() => ({ render() {
    entries = refresh(() => ctx.sessionManager.getEntries(), entries);
    branch = refresh(() => ctx.sessionManager.getBranch(), branch);
    return ['phase:' + ctx.model.name,
      'observed=' + ctx.getContextUsage().tokens + ' oauth=' + ctx.modelRegistry.isUsingOAuth(ctx.model),
      line('entries', entries), line('branch', branch)];
  } }));
});`;

test('self-contained footer reproduces the open unpriced-cost divergence and preserves priced observations', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'powerline-unpriced-'));
  const path = join(directory, 'footer.mjs');
  writeFileSync(path, factory);
  const peer = launch(t, [path], { columns: 140 });
  await peer.init();
  const model_view = { ...host.model_view, name: 'Before', oauth: true };
  const context_usage = tokens => ({ tokens, contextWindow: 272000, percent: tokens / 272000 * 100 });
  await peer.start({ model_view, context_usage: context_usage(0), session_entries: [], session_branch: [] });
  let text = plain(await peer.wait(namedFrame('Before')));
  assert.match(text, /entries:tokens=0 total=0\.000 refused=0/);
  assert.match(text, /branch:tokens=0 total=0\.000 refused=0/);

  const update = async (name, entries, branch, tokens, oauth = true) => {
    peer.notify('context/updated', { resource_owner: owner, host: { ...host,
      model_view: { ...model_view, name, oauth }, context_usage: context_usage(tokens), session_entries: entries, session_branch: branch } });
    const text = plain(await peer.wait(namedFrame(name)));
    assert.match(text, new RegExp('observed=' + tokens + ' oauth=' + oauth));
    return text;
  };
  const toolCall = assistant('call', null, null, [{ ToolCall: { id: 'read-1', name: 'read', arguments_json: '{}', async: false, argument_error: null } }]);
  toolCall.assistant_metadata.stop_reason = 'tool_use';
  const toolResult = { id: 'result', parent: 'call', timestamp_unix_ms: 1001,
    value: { type: 'message', User: { content: [{ ToolResult: { tool_call_id: 'read-1', content: [{ Text: 'result' }], is_error: false } }] } } };
  const mixed = [toolCall, toolResult, assistant('answer', 'result', cost)];
  text = await update('Mixed', mixed, mixed, 123810);
  // Explicitly reproduce the unresolved defect, NOT the desired final footer:
  // optional chaining cannot handle a throwing cost getter, so both cached
  // segments stay at zero despite the separately readable fresh context.
  assert.match(text, /entries:tokens=0 total=0\.000 refused=-32601/);
  assert.match(text, /branch:tokens=0 total=0\.000 refused=-32601/);
  assert.doesNotMatch(text, /total=0\.125/, 'a known subtotal must not become the session total');

  const priced = assistant('priced-branch', null, cost);
  text = await update('Priced branch', [toolCall, toolResult, priced], [priced], 160000);
  assert.match(text, /entries:tokens=0 total=0\.000 refused=-32601/);
  assert.match(text, /branch:tokens=160000 total=0\.125 refused=0/);

  const legacy = [assistant('legacy', null, undefined)];
  text = await update('Missing ledger', legacy, legacy, 180000, false);
  assert.match(text, /entries:tokens=0 total=0\.000 refused=-32601/);
  assert.match(text, /branch:tokens=160000 total=0\.125 refused=-32601/);

  text = await update('Priced', [priced], [priced], 200000);
  assert.match(text, /entries:tokens=200000 total=0\.125 refused=0/);
  assert.match(text, /branch:tokens=200000 total=0\.125 refused=0/);

  const zero = [assistant('zero', null, Object.fromEntries(Object.keys(cost).map(key => [key, 0])))];
  text = await update('Recorded zero', zero, zero, 220000);
  assert.match(text, /entries:tokens=220000 total=0\.000 refused=0/);
  assert.match(text, /branch:tokens=220000 total=0\.000 refused=0/);

  text = await update('Unavailable mirror', null, null, 230000);
  assert.match(text, /entries:tokens=220000 total=0\.000 refused=-32601/);
  assert.match(text, /branch:tokens=220000 total=0\.000 refused=-32601/);
  text = await update('Recovered', [priced], [priced], 240000);
  assert.match(text, /entries:tokens=240000 total=0\.125 refused=0/);
  assert.match(text, /branch:tokens=240000 total=0\.125 refused=0/);
  await peer.close();
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

async function fixture(t, body, command = false) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-presentation-order-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'factory.mjs');
  await writeFile(entry, `export default pi => {
    const component = () => ({ render: () => ['PRESENTATION'], invalidate() {} });
    ${command ? `pi.registerCommand('probe', { handler: async (_, ctx) => { ${body} } });` : `pi.on('session_start', async (_, ctx) => { ${body} });`}
    pi.registerCommand('barrier', { handler() {} });
  };`);
  return entry;
}
test('failed command origin never starts its queued cosmetic mount', async t => {
  const entry = await fixture(t, `ctx.ui.setFooter(component); throw new Error('origin failed');`, true);
  const peer = launch(t, [entry], { hold: ['ui/open'] });
  await peer.init();
  const response = await peer.command('probe').response;
  assert.equal(response.error?.code, -32603, JSON.stringify(response));
  assert.ok((await peer.command('barrier').response).result);
  assert.equal(peer.seen.some(cosmetic), false, 'failed origin must not mount');
  await peer.close();
});

test('reported callback failure discards only that callback presentation', async t => {
  const entry = await fixture(t, '');
  const source = `export default pi => {
    pi.on('session_start', (_, ctx) => { ctx.ui.setFooter(() => ({ render: () => [], invalidate() {} })); throw new Error('callback failed'); });
    pi.on('session_start', (_, ctx) => { ctx.ui.setHeader(() => ({ render: () => ['sibling'], invalidate() {} })); });
  };`;
  await writeFile(entry, source);
  const peer = launch(t, [entry]);
  await peer.init();
  assert.ok((await start(peer).response).result, 'Pi reports callback errors without failing its host hook');
  const header = await peer.wait(frame => cosmetic(frame) && frame.params.placement === 'header');
  await peer.wait(frame => frame.method === 'ui/frame' && frame.params.surface_id === header.params.surface_id);
  assert.equal(peer.seen.some(frame => cosmetic(frame) && frame.params.placement === 'footer'), false);
  await peer.close();
});

const start = peer => peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
const cosmetic = frame => frame.method === 'ui/open' && frame.params.placement !== 'fullscreen';

// The native host binds any reverse call received before the reply to the live
// parent. A local transport option cannot override that wire lifetime. Assert
// reply-before-open, including when a caller awaits Pi's void setter.
test('cosmetic setters return void; origin reply precedes admission and all seed/frame work', async t => {
  const entry = await fixture(t, `
    const results = [ctx.ui.setFooter(component), ctx.ui.setHeader(component), ctx.ui.setWidget('one', ['widget'])];
    console.log('setter-types:' + results.map(value => typeof value).join(','));
    await ctx.ui.setWidget('two', ['awaited widget']);
  `);
  const peer = launch(t, [entry], { hold: ['ui/open'] });
  await peer.init();
  const pending = start(peer);
  assert.ok((await pending.response).result);
  await peer.waitStderr(/setter-types:/);
  assert.match(peer.stderr(), /setter-types:undefined,undefined,undefined/);
  const opens = [];
  for (let i = 0; i < 4; i++) opens.push(await peer.wait(cosmetic));
  const replyIndex = peer.seen.findIndex(frame => frame.id === pending.id && !frame.method);
  for (const open of opens) {
    assert.ok(peer.seen.indexOf(open) > replyIndex, 'native reader must see successful reply first');
    assert.equal(open.params.parent_request_id, pending.id);
    assert.deepEqual(open.params.resource_owner, owner);
    peer.send({ jsonrpc: '2.0', id: open.id, result: { columns: 80, rows: 24 } });
  }
  for (const open of opens) await peer.wait(frame => frame.method === 'ui/frame' && frame.params.surface_id === open.params.surface_id);
  await peer.close();
});

for (const outcome of ['success', 'cancel']) test(`queued presentation waits for ${outcome} origin; fullscreen question stays joined`, async t => {
  const entry = await fixture(t, `
    ctx.ui.setFooter(component);
    await ctx.ui.custom((_tui, _theme, _keys, done) => ({
      render: () => ['QUESTION'], invalidate() {}, handleInput() { done(); }
    }));
  `);
  const peer = launch(t, [entry], { hold: ['ui/open'] });
  await peer.init();
  const pending = start(peer);
  const question = await peer.wait(frame => frame.method === 'ui/open' && frame.params.placement === 'fullscreen');
  assert.equal(peer.seen.some(cosmetic), false, 'cosmetic work cannot acquire a live-parent native lifetime');
  assert.equal(peer.seen.some(frame => frame.id === pending.id && !frame.method), false, 'question must keep hook pending');
  if (outcome === 'cancel') peer.notify('$/cancelRequest', { id: pending.id });
  else {
    peer.send({ jsonrpc: '2.0', id: question.id, result: { columns: 80, rows: 24 } });
    await peer.wait(frame => frame.method === 'ui/frame' && frame.params.surface_id === question.params.surface_id);
    peer.notify('ui/key', { surface_id: question.params.surface_id, key: 'Enter', kind: 'press', modifiers: [] });
  }
  const response = await pending.response;
  if (outcome === 'success') {
    assert.ok(response.result);
    const open = await peer.wait(cosmetic);
    assert.ok(peer.seen.indexOf(open) > peer.seen.indexOf(response));
    peer.send({ jsonrpc: '2.0', id: open.id, result: { columns: 80, rows: 24 } });
    await peer.wait(frame => frame.method === 'ui/frame' && frame.params.surface_id === open.params.surface_id);
  } else {
    assert.equal(response.error.code, -32800);
    assert.ok((await peer.command('barrier').response).result);
    assert.equal(peer.seen.some(cosmetic), false, 'failed/cancelled origin must not start a queued cosmetic mount');
  }
  await peer.close();
});

for (const reason of ['retirement', 'replacement', 'generation', 'shutdown']) test(`queued presentation is revoked by ${reason}`, async t => {
  const entry = await fixture(t, `
    if (ctx.cwd === '/replacement') return;
    ctx.ui.setFooter(component);
    await ctx.ui.custom(() => ({ render: () => ['QUESTION'], invalidate() {} }));
  `);
  const peer = launch(t, [entry], { hold: ['ui/open'] });
  await peer.init();
  const pending = start(peer);
  const question = await peer.wait(frame => frame.method === 'ui/open' && frame.params.placement === 'fullscreen');
  peer.send({ jsonrpc: '2.0', id: question.id, result: { columns: 80, rows: 24 } });
  await peer.wait(frame => frame.method === 'ui/frame' && frame.params.surface_id === question.params.surface_id);
  if (reason === 'shutdown') {
    pending.response.catch(() => {}); // shutdown intentionally has no origin reply
    await peer.close();
  }
  else {
    const replacement = { ...owner, session_id: 'replacement', ...(reason === 'generation' ? { process_generation: owner.process_generation + 1 } : {}) };
    const retiring = reason === 'retirement';
    const response = await peer.request('hook/run', {
      hook: retiring ? 'session_end' : 'session_start', payload: { binding: retiring ? owner : replacement },
      context: retiring ? peer.context() : { ...peer.context(), workspace: '/replacement', resource_owner: replacement },
    }).response;
    assert.ok(response.result, JSON.stringify(response));
    await pending.response;
    await peer.close();
  }
  assert.equal(peer.seen.some(cosmetic), false, 'revoked queued presentation must never reach native admission');
});

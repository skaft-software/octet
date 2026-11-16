import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

async function fixture(t, source) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-liveness-'));
  t.after(() => rm(dir, {recursive:true, force:true}));
  const path = join(dir, 'index.ts'); await writeFile(path, source); return path;
}
const guard = peer => peer.request('hook/run', {hook:'before_tool_call', context:peer.context(), payload:{name:'read', arguments:{}, tool_call_id:'call'}});
const status = async peer => (await peer.request('status/collect', {liveness:true}).response).result.liveness;

test('dispatcher status identifies an unbounded input callback without treating user time as a hang', async t => {
  const path = await fixture(t, `export default pi => pi.on('input', async () => {
    await new Promise(resolve => setTimeout(resolve, 400)); return {action:'transform',text:'accepted'};
  });`);
  const peer = launch(t, [path], {env:{OCTET_PI_HANDLER_BUDGET_MS:'30'}});
  await peer.init(['input_transform_v1','dispatcher_liveness_v1']);
  const pending = peer.request('hook/run', {hook:'before_prompt', context:peer.context(), payload:{phase:'input',text:'original',source:'interactive'}});
  await peer.wait(frame => frame.method === 'status/contribution' && frame.params.liveness?.callbacks.some(c => c.kind === 'input'));
  const snapshot = await status(peer);
  assert.ok(snapshot.last_dispatched_at_ms > 0);
  assert.equal(snapshot.callbacks.find(c => c.kind === 'input').remaining_ms, null);
  assert.equal((await pending.response).result.input_event.text, 'accepted');
  assert.equal((await status(peer)).callbacks.length, 0);
  await peer.close();
});

test('dispatcher user prompts are unbounded and return to a bounded callback after the answer', async t => {
  const path = await fixture(t, `export default pi => pi.on('tool_call', async (_,ctx) => { await ctx.ui.confirm('Continue?','Question'); });`);
  const peer = launch(t, [path], {env:{OCTET_PI_HANDLER_BUDGET_MS:'30'}, hold:['confirmation/request']});
  await peer.init(['remote_ui','dispatcher_liveness_v1']); await peer.start();
  const pending = guard(peer), asked = await peer.wait(frame => frame.method === 'confirmation/request');
  await new Promise(resolve => setTimeout(resolve, 200));
  assert.equal((await status(peer)).callbacks.find(c => c.kind === 'tool_call').remaining_ms, null);
  peer.send({jsonrpc:'2.0',id:asked.id,result:{confirmed:true}});
  assert.equal((await pending.response).result.disposition.action, 'continue');
  await peer.close();
});

test('bounded hang warns, allows a later explicit veto, and is never replayed', async t => {
  const path = await fixture(t, `export default pi => {
    let calls=0;
    pi.on('tool_call', () => { throw Object.assign(Error('PRIVATE callback text'), {code:-32002}); });
    pi.on('tool_call', async () => { if (++calls !== 1) throw Error('replayed'); await new Promise(()=>{}); });
    pi.on('tool_call', () => ({block:true,reason:'explicit veto'}));
  };`);
  const peer = launch(t, [path], {env:{OCTET_PI_HANDLER_BUDGET_MS:'60'}});
  await peer.init(['dispatcher_liveness_v1']);
  for (let i=0;i<2;i++) assert.deepEqual((await guard(peer).response).result.disposition, {action:'deny',reason:'explicit veto'});
  const warning = peer.seen.find(frame => frame.method === 'notification');
  assert.match(warning.params.message, /tool_call.*skipped/);
  assert.ok((await status(peer)).callbacks.every(c => c.interrupted));
  await peer.close();
});

test('a bounded hang without an explicit veto lets the turn continue', async t => {
  const path = await fixture(t, `export default pi => pi.on('tool_call', async () => new Promise(()=>{}));`);
  const peer = launch(t, [path], {env:{OCTET_PI_HANDLER_BUDGET_MS:'60'}});
  await peer.init(['dispatcher_liveness_v1']);
  assert.equal((await guard(peer).response).result.disposition.action, 'continue');
  await peer.close();
});

test('a factory throwing during live load is failed even when it is the only factory', async t => {
  const path = await fixture(t, `export default pi => { pi.on('tool_call',()=>({block:true})); throw Error('PRIVATE factory detail'); };`);
  // Capture deliberately remains strict; live load uses the reviewed declarations.
  const peer = launch(t, [path], {metadata:{hooks:['before_tool_call'], tools:[],commands:[],shortcuts:[]}});
  const initialized = await peer.init(['dispatcher_liveness_v1']);
  assert.ok(initialized.protocol);
  assert.equal((await guard(peer).response).result.disposition.action, 'continue');
  const warning = await peer.wait(frame => frame.method === 'notification');
  assert.equal(warning.params.level, 'error');
  assert.match(warning.params.message, /skipped: failed while loading/);
  assert.ok(!warning.params.message.includes('PRIVATE'));
  await peer.close();
});

test('factory throws on load and reload leave sibling callbacks working', async t => {
  const bad = await fixture(t, `export default pi => { pi.on('tool_call',()=>({block:true,reason:'unaccepted'})); throw Error('PRIVATE factory detail'); };`);
  const good = await fixture(t, `export default pi => pi.on('tool_call',()=>({block:true,reason:'accepted sibling'}));`);
  for (let generation=0;generation<2;generation++) {
    const peer = launch(t, [bad,good], {metadata:{hooks:['before_tool_call','session_start','session_end'],tools:[],commands:[],shortcuts:[]}});
    await peer.init(['dispatcher_liveness_v1','lifecycle_events']);
    assert.equal((await guard(peer).response).result.disposition.reason, 'accepted sibling');
    const warning = await peer.wait(frame => frame.method === 'notification');
    assert.equal(warning.params.level, 'error');
    assert.match(warning.params.message, /skipped: failed while loading/);
    assert.ok(!warning.params.message.includes('PRIVATE'));
    await peer.close();
  }
});

test('completed callbacks accept later events while completion publication waits', async t => {
  const { HookBudget } = await import('../lib/handler-budget.mjs');
  let release;
  const publication = new Promise(resolve => { release = resolve; });
  t.after(() => release());
  const runtime = {
    dispatcher: { async begin() { return {}; }, finish: () => publication },
    backgroundError: error => assert.fail(error),
  };
  const store = { pending: new Set(), controller: new AbortController() };
  const entry = { label: 'stream observer' };
  let calls = 0;
  const run = () => new HookBudget(runtime, store, 'message_update').run(entry, { ...store }, () => ++calls);
  assert.deepEqual(await run(), { ran: true, value: 1 });
  // Completion telemetry is still awaiting its transport ACK, but the handler
  // itself has settled and must receive the next event without replay or loss.
  assert.deepEqual(await run(), { ran: true, value: 2 });
  assert.equal(calls, 2);
});

test('interrupted callback records cannot exhaust healthy dispatcher capacity', async () => {
  const { Dispatcher } = await import('../lib/dispatcher.mjs');
  const dispatcher = new Dispatcher({initialized:false});
  for (let id=0;id<128;id++) {
    const callback = await dispatcher.begin({id}, 'tool_call', 'failed.mjs', 1);
    await dispatcher.interrupt(callback);
  }
  const healthy = await dispatcher.begin({id:129}, 'tool_call', 'healthy.mjs', 1000);
  assert.equal(dispatcher.callbacks.size, 128);
  assert.ok(dispatcher.snapshot().callbacks.some(callback => callback.extension === 'healthy.mjs'));
  await dispatcher.finish(healthy);
});

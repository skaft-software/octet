import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';
import { launch, owner } from './helper.mjs';
import { entryPayload, leafGrant, appendReply } from '../lib/session-leaf.mjs';

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const grant = (id = 'a', head = null) => ({ grant_id: id.repeat(64), activation_epoch: 7, operation_id: 'native-operation', owner, expected_head: head });
const ack = (id = 'native-entry', successor = grant('b', id)) => ({ entry_id: id, head: id, successor });
async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-pi-sync-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'sync.ts');
  await writeFile(entry, `export default pi => {
    function append(mode, ctx) {
      let error, returned, second;
      try {
        returned = pi.appendEntry('private.state', {value:'🙂\\nline\\r\\t', omitted:undefined});
        if (mode === 'twice') second = pi.appendEntry('private.state', {value:'second'});
      } catch(e) { error = e.message; }
      ctx.ui.notify(JSON.stringify({error, voidReturn:returned===undefined, secondVoid:second===undefined,
        entries:ctx.sessionManager.getEntries(), branch:ctx.sessionManager.getBranch(), leaf:ctx.sessionManager.getLeafId()}));
    }
    pi.registerCommand('append', {handler(mode,ctx){append(mode,ctx)}});
    pi.registerCommand('alongside', {async handler(_,ctx){const question=ctx.ui.confirm('Real async question');append('',ctx);ctx.ui.notify('question:'+await question)}});
    pi.on('tool_call', (event,ctx) => {append(event.input.mode,ctx)});
  };`);
  const peer = launch(t, [entry], { hold: ['session/append_entry'] }); await peer.init(['session_entries']);
  peer.snapshot = { session_entries: [], session_branch: [], session_leaf_id: null };
  peer.invoke = (mode = '', leaf = grant()) => peer.request('hook/run', { hook: 'before_tool_call', payload: {name:'test',arguments:{mode}}, context: peer.context(peer.snapshot), ...(leaf === undefined ? {} : {session_leaf:leaf}) });
  peer.report = async () => JSON.parse((await peer.wait(f => f.method === 'notification' && f.params.message.startsWith('{'))).params.message);
  return peer;
}
test('initialized real helper blocks synchronous void append until authoritative delayed ACK, then publishes read-after-write', async t => {
  const peer = await fixture(t), started = Date.now(), parent = peer.invoke();
  const request = await peer.wait(f => f.method === 'session/append_entry');
  assert.equal(request.params.parent_request_id, parent.id); assert.deepEqual(request.params.resource_owner, owner);
  assert.deepEqual(request.params.session_leaf, {grant_id:'a'.repeat(64),activation_epoch:7,operation_id:'native-operation'});
  assert.equal(request.params.data.value, '🙂\nline\r\t'); assert.ok(!Object.hasOwn(request.params.data, 'omitted'));
  await sleep(90); assert.ok(!peer.seen.some(f => f.method === 'notification'));
  peer.send({jsonrpc:'2.0',id:request.id,result:ack()});
  const report = await peer.report(); assert.ok(Date.now() - started >= 90);
  assert.equal(report.error, undefined); assert.equal(report.voidReturn, true);
  assert.equal(report.entries.length, 1); assert.equal(report.entries[0].id, 'native-entry');
  assert.deepEqual(report.entries, report.branch); assert.equal(report.leaf, 'native-entry');
  assert.ok((await parent.response).result); await peer.close();
});
test('known successor alone advances grant; second synchronous request carries exact fences', async t => {
  const peer = await fixture(t), parent = peer.invoke('twice');
  const first = await peer.wait(f => f.method === 'session/append_entry'); peer.send({jsonrpc:'2.0',id:first.id,result:ack()});
  const second = await peer.wait(f => f.method === 'session/append_entry');
  assert.notEqual(first.id, second.id); assert.equal(second.params.session_leaf.grant_id, 'b'.repeat(64));
  assert.equal(second.params.parent_request_id, parent.id);
  peer.send({jsonrpc:'2.0',id:second.id,result:ack('second-entry',null)});
  const report = await peer.report(); assert.equal(report.entries.length, 2); assert.equal(report.error, undefined);
  assert.ok((await parent.response).result); await peer.close();
});
for (const code of [-32601,-32602,-32002,-32800,-32603]) test(`synchronous native refusal ${code} throws without any local success`, async t => {
  const peer = await fixture(t), parent = peer.invoke();
  const request = await peer.wait(f => f.method === 'session/append_entry');
  peer.send({jsonrpc:'2.0',id:request.id,error:{code,message:'authoritative refusal'}});
  const report = await peer.report(); assert.match(report.error,/authoritative refusal/);
  assert.deepEqual(report.entries,[]); assert.deepEqual(report.branch,[]); assert.equal(report.leaf,null);
  assert.ok((await parent.response).result); await peer.close();
});
test('preclaim parent cancellation remains readable while factory blocked, waits for explicit native noncommit', async t => {
  const peer = await fixture(t), parent = peer.invoke();
  const request = await peer.wait(f => f.method === 'session/append_entry'); peer.notify('$/cancelRequest',{id:parent.id});
  const cancel = await peer.wait(f => f.method === '$/cancelRequest'); assert.equal(cancel.params.id, request.id);
  await sleep(40); assert.ok(!peer.seen.some(f => f.method === 'notification'));
  peer.send({jsonrpc:'2.0',id:request.id,error:{code:-32800,message:'cancelled before claim'}});
  assert.deepEqual((await peer.report()).entries, []); assert.equal((await parent.response).error.code,-32800);
  await peer.close();
});
test('afterclaim cancellation waits for actual commit; revoked null successor prevents second append, never replays', async t => {
  const peer = await fixture(t), parent = peer.invoke('twice');
  const request = await peer.wait(f => f.method === 'session/append_entry'); peer.notify('$/cancelRequest',{id:parent.id});
  await peer.wait(f => f.method === '$/cancelRequest'); await sleep(30);
  peer.send({jsonrpc:'2.0',id:request.id,result:ack('committed-after-cancel',null)});
  const report = await peer.report(); assert.equal(report.entries.length,1); assert.equal(report.leaf,'committed-after-cancel');
  assert.match(report.error,/cancelled/); assert.equal((await parent.response).error.code,-32800);
  assert.equal(peer.seen.filter(f => f.method === 'session/append_entry').length,1); await peer.close();
});
test('revocation after known commit returns success but no new leaf authority', async t => {
  const peer = await fixture(t), parent = peer.invoke('twice');
  const request = await peer.wait(f => f.method === 'session/append_entry'); peer.send({jsonrpc:'2.0',id:request.id,result:ack('revoked-entry',null)});
  const report = await peer.report(); assert.equal(report.entries.length,1); assert.match(report.error,/authority exhausted|grant required/);
  assert.ok((await parent.response).result); await peer.close();
});
test('unbound hook append refuses explicitly; ordinary active command waits for existing host persistence reply', async t => {
  const peer = await fixture(t);
  const unbound = peer.request('hook/run',{hook:'before_tool_call',payload:{name:'test',arguments:{}},context:peer.context(peer.snapshot)});
  const report = await peer.report(); assert.match(report.error,/actual host session_leaf grant required/); assert.deepEqual(report.entries,[]);
  assert.ok((await unbound.response).result); assert.equal(peer.seen.filter(f => f.method === 'session/append_entry').length,0);
  const parent = peer.command('append',[],peer.snapshot), request = await peer.wait(f => f.method === 'session/append_entry');
  assert.ok(!Object.hasOwn(request.params,'session_leaf')); peer.send({jsonrpc:'2.0',id:request.id,result:{entry_id:'existing-durable-entry'}});
  assert.equal((await peer.report()).entries[0].id,'existing-durable-entry'); assert.ok((await parent.response).result); await peer.close();
});
for (const update of [{owner:{...owner,process_generation:99}},{activation_epoch:-1},{operation_id:''},{grant_id:'ABC'},{expected_head:undefined}]) test(`invalid host grant is rejected before sending (${Object.keys(update)[0]})`, async t => {
  const peer = await fixture(t), parent = peer.invoke('',{...grant(),...update}); assert.ok((await parent.response).error);
  assert.equal(peer.seen.filter(f => f.method === 'session/append_entry').length,0); await peer.close();
});
for (const result of [{entry_id:'fake'}, {...ack(),head:'other'}, {...ack(),successor:grant('a','native-entry')}, {...ack(),successor:{...grant('b','native-entry'),activation_epoch:8}}, {...ack(),successor:{...grant('b','native-entry'),owner:{...owner,process_generation:3}}}]) test('malformed commit reply is unknown, no optimistic mirror success or replay', async t => {
  const peer = await fixture(t), parent = peer.invoke(); parent.response.catch(()=>{});
  const request = await peer.wait(f => f.method === 'session/append_entry'), exited = once(peer.child,'exit');
  peer.send({jsonrpc:'2.0',id:request.id,result}); assert.equal((await exited)[0],1);
  assert.ok(!peer.seen.some(f => f.method === 'notification')); assert.equal(peer.seen.filter(f => f.method === 'session/append_entry').length,1);
});
test('lost reply EOF wakes blocked caller as unknown and reaps helper worker without replay', async t => {
  const peer = await fixture(t), parent = peer.invoke(); parent.response.catch(()=>{});
  await peer.wait(f => f.method === 'session/append_entry'); const exited = once(peer.child,'exit'); peer.child.stdin.end();
  assert.equal((await exited)[0],1); assert.ok(!peer.seen.some(f => f.method === 'notification')); assert.equal(peer.seen.filter(f => f.method === 'session/append_entry').length,1);
});
test('entry limits match native private envelope: LF/CR/TAB values only, strict controls/UTF8/depth/nodes/bytes/keys', () => {
  assert.equal(entryPayload('custom',{text:'\n\r\t'}).text,'\n\r\t');
  for (const value of ['\0','\x1b','\x7f','\ud800']) assert.throws(()=>entryPayload('custom',{text:value}));
  for (const key of ['a\n','a\r','a\t','x'.repeat(257),'\ud800']) assert.throws(()=>entryPayload('custom',{[key]:1}));
  for (const type of ['','\n','custom\t']) assert.throws(()=>entryPayload(type,{}));
  assert.throws(()=>entryPayload('custom','x'.repeat(16384))); assert.throws(()=>entryPayload('custom',Array(254).fill(0)));
  let deep=0; for(let i=0;i<16;i++) deep={deep}; assert.throws(()=>entryPayload('custom',deep));
  assert.throws(()=>entryPayload('custom',{f(){}})); assert.throws(()=>entryPayload('custom',undefined));
  assert.throws(()=>leafGrant({...grant(),owner:{...owner,extension_instance_id:'foreign'}},owner));
  assert.throws(()=>appendReply(ack('id',{...grant('b','id'),operation_id:'foreign'}),grant(),owner));
});

test('async reverse replies and serialized notifications remain intact alongside a blocking append', async t => {
  const peer=await fixture(t), parent=peer.command('alongside',[],peer.snapshot);
  const question=await peer.wait(f=>f.method==='confirmation/request'), append=await peer.wait(f=>f.method==='session/append_entry');
  peer.send({jsonrpc:'2.0',id:question.id,result:{confirmed:true}});
  await sleep(40);assert.ok(!peer.seen.some(f=>f.method==='notification'));
  peer.send({jsonrpc:'2.0',id:append.id,result:{entry_id:'durable-alongside'}});
  assert.equal((await peer.report()).entries[0].id,'durable-alongside');
  await peer.wait(f=>f.method==='notification'&&f.params.message==='question:true');assert.ok((await parent.response).result);await peer.close();
});
test('shutdown is read while synchronous factory is blocked and child cancellation waits for definitive native refusal', async t => {
  const peer=await fixture(t), parent=peer.invoke();parent.response.catch(()=>{});
  const append=await peer.wait(f=>f.method==='session/append_entry'), exited=once(peer.child,'exit');
  const shutdown=peer.request('shutdown',{}), cancel=await peer.wait(f=>f.method==='$/cancelRequest');assert.equal(cancel.params.id,append.id);
  await sleep(30);assert.ok(!peer.seen.some(f=>f.id===shutdown.id&&!f.method));
  peer.send({jsonrpc:'2.0',id:append.id,error:{code:-32800,message:'shutdown revoked before claim'}});
  assert.deepEqual((await shutdown.response).result,{});assert.equal((await exited)[0],0);
});
test('blocked factory input-pressure admission terminalizes unknown instead of unbounded MessagePort buffering', async t => {
  const peer=await fixture(t), parent=peer.invoke();parent.response.catch(()=>{});
  await peer.wait(f=>f.method==='session/append_entry');const exited=once(peer.child,'exit');
  for(let i=0;i<132;i++) peer.notify('test/pressure',{sequence:i});
  assert.equal((await exited)[0],1);assert.ok(!peer.seen.some(f=>f.method==='notification'));assert.equal(peer.seen.filter(f=>f.method==='session/append_entry').length,1);
});
for(const reply of [{jsonrpc:'2.0',id:'foreign-id',result:{}},{jsonrpc:'2.0',id:'$sync',error:{code:'invalid',message:'bad'}},{jsonrpc:'2.0',id:'$sync',result:'\ud800'}]) test('unknown/malformed replies wake blocked factory as unknown immediately, never waiting or replaying', async t => {
  const peer=await fixture(t), parent=peer.invoke();parent.response.catch(()=>{});
  const append=await peer.wait(f=>f.method==='session/append_entry'), exited=once(peer.child,'exit');
  peer.send({...reply,id:reply.id==='$sync'?append.id:reply.id});
  assert.equal((await exited)[0],1);assert.ok(!peer.seen.some(f=>f.method==='notification'));
});
test('cancellation already read before sync admission is fenced in worker, not lost behind Atomics.wait', async t => {
  const peer=await fixture(t), parent=peer.invoke();peer.notify('$/cancelRequest',{id:parent.id});
  const append=await peer.wait(f=>f.method==='session/append_entry'), cancel=await peer.wait(f=>f.method==='$/cancelRequest');assert.equal(cancel.params.id,append.id);
  peer.send({jsonrpc:'2.0',id:append.id,error:{code:-32800,message:'prevented'}});
  assert.deepEqual((await peer.report()).entries,[]);assert.equal((await parent.response).error.code,-32800);await peer.close();
});

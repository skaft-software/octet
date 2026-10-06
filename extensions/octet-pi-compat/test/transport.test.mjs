import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { launch, root } from './helper.mjs';
import { Transport } from '../lib/transport.mjs';
const sleep = ms => new Promise(r=>setTimeout(r,ms));

for (const executable of [process.execPath, 'bun']) test(`sender sync timeout requests cancellation and waits for committed ACK (${executable === 'bun' ? 'Bun' : 'Node'})`,
  { skip: executable === 'bun' && spawnSync('bun', ['--version']).status !== 0 }, async t => {
  const dir=await mkdtemp(join(tmpdir(),'pi-sync-timeout-')); t.after(()=>rm(dir,{recursive:true,force:true}));
  const script=join(dir,'driver.mjs');
  await writeFile(script,`import {Transport,isolateStdout} from ${JSON.stringify(pathToFileURL(join(root,'lib/transport.mjs')).href)};
    const transport=new Transport(isolateStdout(),{onLost(){process.exit(1)},onMessage(m){
      if(m.method==='initialize') return transport.send({jsonrpc:'2.0',id:m.id,result:{}});
      if(m.method==='append') { const result=transport.requestSync('session/append_entry',{parent_request_id:m.id,entry_type:'test',data:{}},{parent:m.id,timeout:50}); return transport.send({jsonrpc:'2.0',id:m.id,result}); }
      if(m.method==='shutdown') return transport.send({jsonrpc:'2.0',id:m.id,result:{}}).then(()=>transport.idle()).then(()=>transport.close()).then(()=>process.exit(0));
    }}); transport.start();`);
  const child=spawn(executable,[script],{stdio:['pipe','pipe','pipe']}); t.after(()=>child.kill());
  const frames=[]; let stderr='';child.stderr.on('data',b=>stderr+=b); child.stdin.on('error',()=>{});
  createInterface({input:child.stdout}).on('line',line=>frames.push(JSON.parse(line)));
  const send=message=>child.stdin.write(JSON.stringify({jsonrpc:'2.0',...message})+'\n');
  async function wait(predicate) { for(let i=0;i<1000;i++){const frame=frames.find(predicate);if(frame)return frame;await sleep(5)}assert.fail(`missing frame ${stderr}`); }
  send({id:1,method:'initialize',params:{}});await wait(f=>f.id===1);
  send({id:2,method:'append',params:{}});const request=await wait(f=>f.method==='session/append_entry');
  const cancel=await wait(f=>f.method==='$/cancelRequest'); assert.equal(cancel.params.id,request.id);
  await sleep(70);assert.ok(!frames.some(f=>f.id===2));
  send({id:request.id,result:{entry_id:'native-commit-after-sender-timeout'}});
  assert.deepEqual((await wait(f=>f.id===2)).result,{entry_id:'native-commit-after-sender-timeout'});
  assert.equal(frames.filter(f=>f.method==='session/append_entry').length,1);
  const exited=once(child,'exit');send({id:3,method:'shutdown',params:{}}); await wait(f=>f.id===3);assert.equal((await exited)[0],0);
});
for (const bytes of [Buffer.from([0xff,10]), Buffer.from('{"jsonrpc":"2.0","id":"pi:unknown","result":{}}\n'), Buffer.from('{"jsonrpc":"2.0","id":1,"result":{},"error":{}}\n'), Buffer.from('{"jsonrpc":"2.0","id":1,"error":{"code":"bad","message":"bad"}}\n'), Buffer.from('{"jsonrpc":"2.0","id":"pi:1","result":"\\ud800"}\n')]) test('canonical worker reader rejects invalid UTF8/envelope/error/unknown replies', async t => {
  const peer=launch(t); await peer.init();const exited=once(peer.child,'exit');peer.child.stdin.write(bytes);assert.equal((await exited)[0],1);
});
test('transport bounds async pending catalog and both writer bytes and frames before memory can grow', async () => {
  let lost;
  const transport=new Transport({write(){},output:{}},{onLost(e){lost=e}});
  const pending=[];for(let i=0;i<6;i++) pending.push(transport.send({text:'x'.repeat(900000)}).catch(()=>{}));
  assert.match(lost.message,/writer queue/);
  await Promise.all(pending); // Includes the held in-flight frame, not only queued frames.
  const catalog=new Transport({write(_line,callback){callback()},output:{}},{onLost(){}});
  for(let i=0;i<128;i++) catalog.request('test',{}).catch(()=>{});
  assert.throws(()=>catalog.request('test',{}),/host request catalog/);catalog.fail(new Error('fixture done'));
});

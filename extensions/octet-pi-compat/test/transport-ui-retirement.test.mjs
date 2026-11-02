import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { root } from './helper.mjs';

test('pending closures predate the next sync call while ordinary notification handlers remain FIFO and nonreentrant', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-sync-retirement-'));
  t.after(() => rm(dir, {recursive:true,force:true}));
  const script = join(dir, 'probe.mjs');
  await writeFile(script, `import {Transport,isolateStdout} from ${JSON.stringify(pathToFileURL(join(root,'lib/transport.mjs')).href)};
    const events=[];
    const transport=new Transport(isolateStdout(), {onLost(){process.exit(1)},onMessage(m){
      if(m.method==='probe') {
        events.push('enter');
        const first=transport.requestSync('ui/chrome',{}, {onUiClosed:id=>events.push('first:'+id)});
        events.push('between');
        const second=transport.requestSync('ui/chrome',{}, {onUiClosed:id=>events.push('second:'+id)});
        events.push('leave');
        return transport.send({jsonrpc:'2.0',id:m.id,result:{events:[...events],first,second}});
      }
      if(m.method==='barrier') return transport.send({jsonrpc:'2.0',id:m.id,result:[...events]});
      if(m.method==='ui/closed') events.push('closed-normal');
      if(m.method==='control') events.push(m.params.label);
      if(m.method==='shutdown') return transport.send({jsonrpc:'2.0',id:m.id,result:{}}).then(()=>transport.idle()).then(()=>transport.close()).then(()=>process.exit(0));
    }}); transport.start();`);
  const child = spawn(process.execPath, [script], {stdio:['pipe','pipe','pipe']});
  t.after(() => child.kill());
  let stderr = ''; child.stderr.on('data', b => stderr += b);
  child.stdin.on('error', () => {});
  const frames = [], waiters = [];
  createInterface({input:child.stdout}).on('line', line => {
    const frame = JSON.parse(line), index = waiters.findIndex(w => w.match(frame));
    if (index < 0) frames.push(frame);
    else { const [waiter] = waiters.splice(index,1); clearTimeout(waiter.timer); waiter.resolve(frame); }
  });
  const send = frame => child.stdin.write(JSON.stringify({jsonrpc:'2.0',...frame})+'\n');
  const wait = match => {
    const index = frames.findIndex(match);
    if (index >= 0) return Promise.resolve(frames.splice(index,1)[0]);
    return new Promise((resolve,reject) => {
      const waiter = {match,resolve,timer:setTimeout(()=>reject(new Error('RPC timeout: '+stderr)),7000)};
      waiters.push(waiter);
    });
  };
  send({id:1,method:'probe',params:{}});
  const first = await wait(f => f.method === 'ui/chrome');
  child.stdin.write([
    {jsonrpc:'2.0',method:'control',params:{label:'control-before'}},
    {jsonrpc:'2.0',method:'ui/closed',params:{surface_id:'surface-1',reason:'reload'}},
    {jsonrpc:'2.0',method:'control',params:{label:'control-after'}},
    {jsonrpc:'2.0',id:first.id,result:{committed:'first'}},
  ].map(JSON.stringify).join('\n')+'\n');
  const second = await wait(f => f.method === 'ui/chrome');
  send({id:second.id,result:{committed:'second'}});
  const observed = (await wait(f => f.id === 1)).result;
  assert.deepEqual(observed, {events:['enter','first:surface-1','between','second:surface-1','leave'],first:{committed:'first'},second:{committed:'second'}});
  send({id:2,method:'barrier',params:{}});
  assert.deepEqual((await wait(f => f.id === 2)).result, [...observed.events,'control-before','closed-normal','control-after']);
  const exited = once(child,'exit');
  send({id:3,method:'shutdown',params:{}});
  assert.deepEqual((await wait(f => f.id === 3)).result, {});
  assert.equal((await exited)[0], 0, stderr);
});

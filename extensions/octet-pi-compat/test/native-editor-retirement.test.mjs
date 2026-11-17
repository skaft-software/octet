import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

const retired = { code: -32002, message: 'not_foreground_owner: native editor terminal or surface lease retired' };
const observation = text => ({ text, cursor: {line:0,col:text.length}, cursor_offset:text.length,
  padding_x:0, autocomplete_max_visible:5, pastes:[], atoms:[], undo_history:[], autocomplete:null });

async function fixture(t, mode = 'native') {
  const dir = await mkdtemp(join(tmpdir(), 'octet-editor-retirement-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'retirement.ts');
  await writeFile(entry, `import { Editor } from '@earendil-works/pi-tui';
    export default pi => {
      const events = []; let finish;
      pi.registerCommand('retirement-open', { handler: async (_, ctx) => {
        await ctx.ui.custom((tui, theme, _kb, done) => {
          finish = done;
          const editor = new class extends Editor {
            render() { return ['ready']; }
            dispose() { events.push('dispose'); super.dispose(); if (${JSON.stringify(mode)} === 'dispose-error') throw new Error('genuine dispose failure'); }
          }(tui, {borderColor:s=>s});
          tui.addChild(editor);
          // Paint now reads local observations only. Exercise the same synchronous
          // retirement boundary in a factory callback, before painting begins.
          events.push('callback-enter');
          try {
            if (!['close', 'dispose-error'].includes(${JSON.stringify(mode)})) {
              if (${JSON.stringify(mode)} === 'forged') throw Object.assign(new Error(${JSON.stringify(retired.message)}), {code:-32002});
              try { editor.getText(); }
              catch (error) { if (${JSON.stringify(mode)} === 'after') throw new Error('genuine callback failure after retirement'); throw error; }
            }
          } finally { events.push('callback-leave'); }
          return editor;
        });
      }});
      pi.registerCommand('retirement-finish', {handler: async () => { finish(); }});
      pi.registerTool({name:'retirement_probe',description:'Observe cleanup order',parameters:{type:'object'},
        async execute(){return {content:[{type:'text',text:JSON.stringify(events)}]};}});
    };`);
  const peer = launch(t, [entry], { hold: ['ui/chrome'] });
  await peer.init();
  const opened = peer.command('retirement-open');
  const open = await peer.wait(f => f.method === 'ui/open');
  const create = await peer.wait(f => f.method === 'ui/chrome');
  assert.equal(create.params.chrome.operation.op, 'create');
  peer.send({ jsonrpc: '2.0', id: create.id, result: {editor_id:'native-1'} });
  const initial = await peer.wait(f => f.method === 'ui/chrome');
  assert.equal(initial.params.chrome.operation.field, 'paint');
  peer.send({ jsonrpc:'2.0', id:initial.id, result:{value:observation('')} });
  return { peer, opened, id:open.params.surface_id };
}

async function probe(peer) {
  const reply = await peer.request('tool/call', { name:'retirement_probe', arguments:{}, context:peer.context() }).response;
  assert.ok(reply.result, JSON.stringify(reply));
  return JSON.parse(reply.result.content[0].text);
}

for (const success of [false, true, 'large']) test(`an earlier host ui/closed is observed before a synchronous ${success === 'large' ? 'near-limit success' : success ? 'success' : 'refusal'} without reentrant disposal`, async t => {
  const { peer, opened, id } = await fixture(t);
  const read = await peer.wait(f => f.method === 'ui/chrome');
  assert.equal(read.params.chrome.operation.op, 'read');
  peer.child.stdin.write([
    {jsonrpc:'2.0',method:'ui/closed',params:{surface_id:id,reason:'reload'}},
    {jsonrpc:'2.0',id:read.id,...success ? {result:{value:observation(success === 'large' ? 'x'.repeat(1048200) : 'last authorized read')}} : {error:retired}},
  ].map(JSON.stringify).join('\n') + '\n');
  assert.ok((await opened.response).result);
  assert.deepEqual(await probe(peer), ['callback-enter','callback-leave','dispose']);
  peer.notify('ui/closed', {surface_id:id,reason:'duplicate observation'});
  assert.deepEqual(await probe(peer), ['callback-enter','callback-leave','dispose']);
  assert.equal(peer.seen.some(f => f.method === 'notification' || f.method === 'ui/close' || f.method === 'ui/frame'), false);
  assert.equal(peer.seen.filter(f => f.method === 'ui/chrome').length, 3);
  await peer.close();
});

for (const mode of ['foreign', 'ordinary', 'bounds', 'forged', 'after']) test(`retirement observation preserves genuine errors: ${mode}`, async t => {
  const { peer, opened, id } = await fixture(t, mode);
  let read;
  if (mode !== 'forged') read = await peer.wait(f => f.method === 'ui/chrome');
  if (read) {
    const error = mode === 'bounds' ? {code:-32602,message:'invalid_request: editor mount mismatch'} : retired;
    const frames = [];
    if (!['ordinary','forged'].includes(mode)) frames.push({jsonrpc:'2.0',method:'ui/closed',params:{surface_id:mode === 'foreign' ? 'foreign-surface' : id,reason:'reload'}});
    frames.push({jsonrpc:'2.0',id:read.id,error});
    peer.child.stdin.write(frames.map(JSON.stringify).join('\n') + '\n');
  }
  if (['ordinary','foreign','forged'].includes(mode)) {
    const retain = await peer.wait(f => f.method === 'ui/chrome');
    assert.equal(retain.params.chrome.operation.field, 'paint');
    peer.send({jsonrpc:'2.0',id:retain.id,result:{value:observation('draft')}});
    await peer.wait(f => f.method === 'ui/close');
  }
  const reply = await opened.response;
  assert.ok(reply.error, JSON.stringify(reply));
  assert.match(reply.error.message, mode === 'after' ? /genuine callback failure after retirement/ : mode === 'bounds' ? /editor mount mismatch/ : /native editor terminal or surface lease retired/);
  assert.deepEqual(await probe(peer), ['callback-enter','callback-leave','dispose']);
  await peer.close();
});

for (const mode of ['close', 'dispose-error']) test(`retirement during final-state retention skips further reads and duplicate close: ${mode}`, async t => {
  const { peer, opened, id } = await fixture(t, mode);
  assert.ok((await opened.response).result);
  const finishing = peer.command('retirement-finish');
  const read = await peer.wait(f => f.method === 'ui/chrome');
  assert.equal(read.params.chrome.operation.op, 'read');
  peer.child.stdin.write([
    {jsonrpc:'2.0',method:'ui/closed',params:{surface_id:id,reason:'reload'}},
    {jsonrpc:'2.0',id:read.id,error:retired},
  ].map(JSON.stringify).join('\n') + '\n');
  assert.ok((await finishing.response).result);
  if (mode === 'dispose-error') await peer.wait(f => f.method === 'notification' && /genuine dispose failure/.test(f.params.message));
  assert.deepEqual(await probe(peer), ['callback-enter','callback-leave','dispose']);
  assert.equal(peer.seen.some(f => f.method === 'ui/close'), false);
  assert.equal(peer.seen.filter(f => f.method === 'ui/chrome').length, 3);
  assert.equal(peer.seen.filter(f => f.method === 'notification').length, mode === 'dispose-error' ? 1 : 0);
  await peer.close();
});

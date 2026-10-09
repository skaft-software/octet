import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configure } from '../configure.mjs';
import { Runtime } from '../lib/runtime.mjs';
import { plainJSON } from '../lib/errors.mjs';
import { host, launch } from './helper.mjs';

const feature = 'builtin_tool_overrides_v1';
function factory(t, source) {
  const directory = mkdtempSync(join(tmpdir(), 'octet-builtin-overrides-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.ts');
  writeFileSync(entry, source);
  return { directory, entry };
}
const tool = (name, description = 'replacement') => `{
  name:${JSON.stringify(name)},description:${JSON.stringify(description)},parameters:{type:'object'},
  execute(){return {content:[{type:'text',text:${JSON.stringify(description)}}]};}
}`;
function initialize(peer, grants, features = [feature, 'dynamic_tools']) {
  const metadata = peer.metadata;
  return peer.request('initialize', {
    api_version: '0.4', workspace: process.cwd(), host,
    ...(grants === undefined ? {} : { capabilities: { builtin_tool_overrides: grants } }),
    contributes: { tools: metadata.tools.map(t => t.name), commands: metadata.commands.map(c => c.name),
      hooks: metadata.hooks, shortcuts: metadata.shortcuts, tool_renderers: metadata.tool_renderers },
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'],
      optional_features: features, limits: { max_concurrent_requests: 8 } },
  }).response;
}

test('reviewed capture emits only exact captured builtin grants, never config-derived authority', t => {
  const { directory, entry } = factory(t, `export default pi => {
    pi.registerTool(${tool('write')}); pi.registerTool(${tool('read')});
    pi.registerTool(${tool('external')}); pi.registerTool(${tool('read', 'last Pi registration wins')});
  };`);
  const output = join(directory, 'octet-pi-compat');
  assert.throws(() => configure({ output, extensions: [entry] }), /--reviewed/);
  const { registrations } = configure({ reviewed: true, output, extensions: [entry] });
  const manifest = readFileSync(join(output, 'extension.toml'), 'utf8');
  assert.match(manifest, /\[capabilities\][\s\S]*builtin_tool_overrides = \["read", "write"\][\s\S]*\[contributes\]/);
  assert.equal(registrations.tools.find(t => t.name === 'read').description, 'last Pi registration wins');
  assert.equal(JSON.parse(readFileSync(join(output, 'bridge.json'), 'utf8')).builtin_tool_overrides, undefined);
});

test('ordinary reviewed factories do not request builtin authority', t => {
  const { directory, entry } = factory(t, `export default pi => pi.registerTool(${tool('external')});`);
  configure({ reviewed: true, output: join(directory, 'octet-pi-compat'), extensions: [entry] });
  assert.doesNotMatch(readFileSync(join(directory, 'octet-pi-compat/extension.toml'), 'utf8'), /builtin_tool_overrides/);
});

test('startup overrides require the exact native grant and offered feature before publication', async t => {
  const { entry } = factory(t, `export default pi => pi.registerTool(${tool('read')});`);
  for (const [grants, features, message] of [
    [undefined, [], /builtin_tool_overrides_v1/],
    [[], [feature], /builtin_tool_overrides_v1/],
    [['write'], [feature], /read.*not reviewed/],
    [['read'], [], /builtin_tool_overrides_v1/],
  ]) {
    const peer = launch(t, [entry]);
    const reply = await initialize(peer, grants, features);
    assert.match(reply.error?.message ?? '', message);
    assert.equal(peer.seen.some(frame => frame.method === 'tools/register'), false);
    await peer.close();
  }
  const peer = launch(t, [entry]);
  const reply = await initialize(peer, ['read']);
  assert.ok(reply.result, JSON.stringify(reply));
  assert.ok(reply.result.protocol.features.includes(feature));
  assert.equal(reply.result.tools.filter(t => t.name === 'read').length, 1);
  const call = await peer.request('tool/call', { name: 'read', arguments: {}, context: peer.context() }).response;
  assert.equal(call.result.content[0].text, 'replacement');
  await peer.close();
});

test('initialize validates bounded unique native grants and does not select an ungranted feature', async () => {
  for (const grants of [null, {}, 'read', ['read', 'read'], ['Read'], ['web_search'], ['read\n'], Array(7).fill('read')]) {
    const runtime = new Runtime({ extensions: [] }, {});
    try {
      await assert.rejects(runtime.initialize({ api_version: '0.4', capabilities: { builtin_tool_overrides: grants },
        protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: [feature] } }, { id: 1 }), /builtin.*grant/);
      assert.equal(runtime.loaded, false);
    } finally { runtime.uninstallChildren(); }
  }
  const runtime = new Runtime({ extensions: [] }, {});
  try {
    const result = await runtime.initialize({ api_version: '0.4', capabilities: {},
      protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: [feature] } }, { id: 1 });
    assert.equal(result.protocol.features.includes(feature), false);
  } finally { runtime.uninstallChildren(); }
});

test('Pi tool details omit absent object fields in final, progress and hook results without weakening JSON validation', async t => {
  const { entry } = factory(t, `export default pi => {
    pi.registerTool({ name:'details',description:'Optional details',parameters:{type:'object'},execute(id,args,signal,update) {
      const details={truncation:undefined,nested:{absent:undefined,value:null},rows:[{absent:undefined,value:1}]};
      update({content:[],details}); details.nested.value='after snapshot'; return {content:[],details};
    }});
    pi.on('tool_result',()=>({details:{absent:undefined,value:null}}));
  };`);
  const peer = launch(t, [entry]); await peer.init(['request_progress']);
  const call = peer.request('tool/call', { name: 'details', arguments: {}, context: peer.context() });
  const result = await call.response;
  assert.ok(result.result, JSON.stringify(result));
  const progress = await peer.wait(frame => frame.method === '$/progress');
  assert.deepEqual(progress.params.event.result.metadata.pi_details, { nested: { value: null }, rows: [{ value: 1 }] });
  assert.deepEqual(result.result.metadata.pi_details, { nested: { value: 'after snapshot' }, rows: [{ value: 1 }] });
  const hook = await peer.request('hook/run', { hook: 'after_tool_call', payload: {
    name: 'details', arguments: {}, output: '', is_error: false }, context: peer.context() }).response;
  assert.deepEqual(hook.result.tool_result.metadata.pi_details, { value: null });
  await peer.close();
  assert.throws(() => plainJSON({ missing: undefined }, 'ordinary JSON'), /plain JSON/);
  for (const value of [[undefined], { value: NaN }, { value: new Date() }, { value() {} }]) {
    assert.throws(() => plainJSON(value, 'details', 65536, { omitUndefined: true }), /plain JSON/);
  }
});

test('bridge.json cannot supply the native override grant', async t => {
  const { directory, entry } = factory(t, `export default pi => pi.registerTool(${tool('read')});`);
  const config = join(directory, 'bridge.json');
  writeFileSync(config, JSON.stringify({ extensions: [entry], builtin_tool_overrides: ['read'] }));
  const peer = launch(t, [entry], { config });
  const reply = await initialize(peer, [], [feature]);
  assert.match(reply.error?.message ?? '', /builtin_tool_overrides_v1/);
  await peer.close();
});

test('late builtin registration cannot widen grants, bypass ACKs, or replace state on host refusal', async t => {
  const { entry } = factory(t, `export default pi => {
    pi.registerTool(${tool('read', 'original')});
    pi.registerCommand('allowed',{handler(){pi.registerTool(${tool('read', 'new')});}});
    pi.registerCommand('unreviewed',{handler(){pi.registerTool(${tool('write')});}});
  };`);
  const peer = launch(t, [entry], { auto: false });
  assert.ok((await initialize(peer, ['read'])).result);
  const unreviewed = await peer.command('unreviewed').response;
  assert.match(unreviewed.error.message, /write.*not reviewed/);
  assert.equal(peer.seen.some(frame => frame.method === 'tools/register'), false);
  const attempt = peer.command('allowed');
  const refused = await peer.wait(frame => frame.method === 'tools/register');
  assert.equal(refused.params.tools[0].name, 'read');
  peer.send({ jsonrpc: '2.0', id: refused.id, error: { code: -32602, message: 'native owner conflict' } });
  assert.match((await attempt.response).error.message, /native owner conflict/);
  const read = () => peer.request('tool/call', { name: 'read', arguments: {}, context: peer.context() }).response;
  assert.equal((await read()).result.content[0].text, 'original');
  const accepted = peer.command('allowed');
  const request = await peer.wait(frame => frame.method === 'tools/register');
  peer.send({ jsonrpc: '2.0', id: request.id, result: { revision: 1, tools: ['read'] } });
  assert.ok((await accepted.response).result);
  assert.equal((await read()).result.content[0].text, 'new');
  await peer.close();
});

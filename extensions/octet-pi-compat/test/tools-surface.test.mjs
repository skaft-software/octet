import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { validateTool, toolWire, registerTool, toolContent, executeRegisteredTool, getAllTools, setActiveTools, prepareRegisteredArguments, prepareToolLoadout, createToolContext } from '../lib/tools.mjs';
import { launch } from './helper.mjs';

function definition(extra = {}) {
  return { name: 'probe', label: 'Probe', description: 'Probe tool', parameters: { type: 'object' },
    execute: async () => ({ content: [{ type: 'text', text: 'ok' }], details: undefined }), ...extra };
}
function runtime() {
  const store = { id: 2, state: { owner: { session_id: 's', extension_instance_id: 'i', process_generation: 1 } }, live: true, controller: new AbortController() };
  return { loaded: false, tools: new Map(), features: new Set(['request_progress', 'dynamic_tools', 'active_tools', 'artifacts']),
    scope: new AsyncLocalStorage(), current: () => store, require: () => {}, track: p => p,
    flush: async () => {}, store };
}

test('module: Pi schema and sampling fields are mapped, no inert unsupported definition fields', () => {
  const tool = definition({ outputSchema: { type: 'object' }, defaultActive: false,
    constrainedSampling: { type: 'json_schema', strict: 'require' }, annotations: { readOnlyHint: true }, namespace: { name: 'docs', instructions: 'Use docs' } });
  validateTool(tool);
  const wire = toolWire('probe', { definition: tool });
  assert.deepEqual(wire.output_schema, tool.outputSchema);
  assert.equal(wire.default_active, false);
  assert.deepEqual(wire.constrained_sampling, tool.constrainedSampling);
  assert.equal(wire.outputSchema, undefined);
  validateTool(definition({ prepareArguments: x => x, prepareLoadout: () => ({}) }));
  for (const field of [{ executionMode: 'parallel' }, { exposure: 'hidden' }, { output_schema: {} }]) {
    assert.throws(() => validateTool(definition(field)), /unsupported_feature/);
  }
});

test('module: unsupported tool schema constraints fail before publication without being stripped', () => {
  const parameters = { type: 'object', properties: { pattern: { type: 'string' }, nested: { type: 'array', items: { anyOf: [{ type: 'string' }, { type: 'null' }] } } } };
  validateTool(definition({ parameters }));
  const bad = structuredClone(parameters); bad.properties.nested.items.anyOf[0].pattern = '^[a-z]+$';
  assert.throws(() => validateTool(definition({ parameters: bad })), /anyOf\[0\].pattern.*not supported/);
  assert.equal(bad.properties.nested.items.anyOf[0].pattern, '^[a-z]+$');
  assert.throws(() => validateTool(definition({ parameters: { type: 'object', deprecated: 'yes' } })), /deprecated.*boolean/);
  assert.throws(() => validateTool(definition({ parameters: { type: 'array', items: false } })), /must be a schema object/);
});

test('module: runtime registration waits for an actual host ACK, replaces names, and preserves local state on refusal', () => {
  const rt = runtime(); registerTool(rt, '/actual/probe.ts', definition());
  rt.loaded = true; let calls = 0;
  rt.transport = { requestSync(method, params) { assert.equal(method, 'tools/register'); assert.equal(params.parent_request_id, undefined); calls++; return { revision: calls, tools: ['probe'] }; } };
  const replacement = definition({ description: 'replacement' });
  assert.equal(registerTool(rt, '/actual/probe.ts', replacement), undefined);
  assert.equal(rt.tools.get('probe').definition, replacement);
  rt.transport.requestSync = () => { throw new Error('host refused'); };
  assert.throws(() => registerTool(rt, '/actual/probe.ts', definition()), /host refused/);
  assert.equal(rt.tools.get('probe').definition, replacement);
});

test('module: selection and catalog queries use authoritative host state and consume author hints', () => {
  const rt = runtime(); registerTool(rt, '/actual/probe.ts', definition({ annotations: { readOnlyHint: true }, namespace: { name: 'docs' } }));
  const calls = [];
  rt.transport = { requestSync(method, params) { calls.push([method, params]); return method === 'tools/snapshot'
    ? { active_tools: [], all_tools: [{ name: 'probe', description: 'host description', parameters: { type: 'object' }, exposure: 'direct' }] } : {}; } };
  const all = getAllTools(rt, '/actual/probe.ts');
  assert.equal(all[0].description, 'host description'); assert.equal(all[0].annotations.readOnlyHint, true);
  assert.equal(all[0].namespace.name, 'docs'); assert.equal(all[0].sourceInfo.path, '/actual/probe.ts');
  assert.equal(setActiveTools(rt, '/actual/probe.ts', ['unknown', 'probe', 'probe']), undefined);
  assert.deepEqual(calls.at(-1)[1].names, ['probe']);
});

test('module: images are published with exact digest and ownership through the existing artifact service', async () => {
  const rt = runtime(), data = Buffer.from('fixture').toString('base64');
  rt.hostCall = async (method, params, store) => {
    assert.equal(method, 'artifact/publish'); assert.equal(store, rt.store);
    assert.equal(params.size, 7); assert.equal(params.sha256, createHash('sha256').update('fixture').digest('hex'));
    assert.equal(params.data.data, data); return { artifact_id: 'verified-artifact' };
  };
  assert.deepEqual(await toolContent(rt, [{ type: 'image', data, mimeType: 'image/png' }], rt.store),
    [{ type: 'image', artifact_id: 'verified-artifact', mime_type: 'image/png' }]);
  await assert.rejects(toolContent(rt, [{ type: 'image', data: 'not base64!', mimeType: 'image/png' }], rt.store), /base64/);
});

test('module: structuredContent, explicit errors, details, empty content and ignored late updates', async () => {
  const rt = runtime(); let late;
  rt.transport = { notify: async () => { throw new Error('late update must not publish'); } };
  registerTool(rt, '/probe.ts', definition({ execute: async (_id, _args, _signal, update) => { late = update; return { content: [], details: { rows: 1 }, structuredContent: null, isError: true }; } }));
  const result = await executeRegisteredTool(rt, { name: 'probe', arguments: {} }, rt.store, {});
  assert.deepEqual(result, { content: [], is_error: true, metadata: { pi_details: { rows: 1 } }, structured_content: null });
  late({ content: [{ type: 'text', text: 'too late' }], details: { ignored: true } });
});

test('module: argument preparation is a distinct synchronous callback, not an execute-time shim', () => {
  const rt = runtime(); registerTool(rt, '/probe.ts', definition({ prepareArguments: raw => ({ count: Number(raw.count) }) }));
  assert.equal(toolWire('probe', rt.tools.get('probe')).prepare_arguments, true);
  assert.deepEqual(prepareRegisteredArguments(rt, { name: 'probe', arguments: { count: '7' } }, rt.store), { arguments: { count: 7 } });
  rt.tools.get('probe').definition.prepareArguments = async raw => raw;
  assert.throws(() => prepareRegisteredArguments(rt, { name: 'probe', arguments: {} }, rt.store), /plain JSON/);
});

test('module: loadout callbacks receive the actual host registry and only project descriptions/omissions', () => {
  const rt = runtime(), original = [{ name: 'probe', description: 'original', parameters: { type: 'object' } }, { name: 'read', description: 'native read', parameters: { type: 'object' } }];
  registerTool(rt, '/probe.ts', definition({ prepareLoadout: loadout => {
    assert.deepEqual(loadout.registered.map(tool => tool.name), ['probe', 'read', 'inactive']);
    assert.deepEqual(loadout.callable.map(tool => tool.name), ['probe', 'read']);
    assert.equal(loadout.getExposure('read'), 'direct');
    return { descriptions: { probe: 'reads native tools' }, hiddenDeclarations: ['read'] };
  } }));
  rt.transport = { requestSync() { return { all_tools: [...original, { name: 'inactive', description: 'not active' }].map(tool => ({ ...tool, exposure: 'direct' })), active_tools: ['probe', 'read'] }; } };
  rt.backgroundError = error => { throw error; };
  assert.deepEqual(prepareToolLoadout(rt, rt.store, original), [{ ...original[0], description: 'reads native tools' }]);
  assert.equal(original[0].description, 'original'); assert.equal(original.length, 2);
});

test('module: nested execution returns real full outcomes and uses the frozen host catalog', async () => {
  const rt = runtime(); rt.features.add('tool_composition_v1'); registerTool(rt, '/probe.ts', definition());
  let failed = false;
  rt.transport = {
    requestSync(method) { assert.equal(method, 'composition/context'); return { tools: [{ name: 'read', description: 'real read', parameters: { type: 'object' } }] }; },
    async request(method, params, options) {
      assert.equal(method, 'composition/call'); assert.equal(params.full_outcome, true); assert.equal(params.parent_request_id, 2); assert.equal(options.parent, 2);
      return { value: { tool_call: { id: 'host-call/1', name: params.name, arguments: params.arguments }, content: [{ Text: failed ? 'native denied' : 'read output' }],
        is_error: failed, ...(failed ? {} : { metadata: { pi_details: { rows: 1 } }, structured_content: null }) } };
    },
  };
  const context = await createToolContext(rt, { name: 'probe' }, rt.store, {});
  assert.equal(context.tools[0].name, 'read'); assert.equal(typeof context.tools[0].execute, 'function');
  const outcome = await context.executeTool('read', { path: 'file' });
  assert.equal(outcome.toolCall.id, 'host-call/1'); assert.deepEqual(outcome.result.details, { rows: 1 }); assert.equal(outcome.result.structuredContent, null);
  failed = true; const error = await context.executeTool('missing', {});
  assert.equal(error.isError, true); assert.equal(error.result.content[0].text, 'native denied');
});

async function fixture(t, source) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-tools-surface-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'probe.ts'); await writeFile(entry, source); return entry;
}

test('adapter: Pi outputSchema and structuredContent survive a real factory process', async t => {
  const entry = await fixture(t, `export default pi => pi.registerTool({ name:'probe',label:'Probe',description:'Probe',parameters:{type:'object'},
    outputSchema:{type:'object',properties:{count:{type:'integer'}},required:['count']},
    async execute(){return {content:[{type:'text',text:'one'}],details:{row:1},structuredContent:{count:1}}} });`);
  const peer = launch(t, [entry]); const initialized = await peer.init([]);
  assert.equal(initialized.tools[0].output_schema.type, 'object');
  const call = await peer.request('tool/call', { name: 'probe', arguments: {}, context: peer.context() }).response;
  assert.deepEqual(call.result.structured_content, { count: 1 }); assert.deepEqual(call.result.metadata.pi_details, { row: 1 });
  await peer.close();
});

test('adapter: late callbacks use subscribed native hooks and dispatch snapshots', async t => {
  const entry = await fixture(t, `export default pi => { let count=0;
    pi.on('tool_call',()=>{});
    pi.registerCommand('load',{handler:()=>{pi.on('tool_call',()=>{count++;pi.on('tool_call',()=>{count+=100})})}});
    pi.registerCommand('check',{handler:args=>{if(count!==Number(args))throw Error('unexpected count '+count)}});
  };`);
  const peer = launch(t, [entry]); await peer.init([]);
  assert.ok((await peer.command('load').response).result);
  const event = () => peer.request('hook/run', { hook: 'before_tool_call', payload: { name: 'probe', arguments: {} }, context: peer.context() }).response;
  assert.ok((await event()).result); assert.ok((await peer.command('check', ['1']).response).result);
  assert.ok((await event()).result); assert.ok((await peer.command('check', ['102']).response).result);
  await peer.close();
});

test('adapter: late registration uses the native dynamic catalog protocol rather than a static manifest lock', async t => {
  const entry = await fixture(t, `export default pi => { pi.registerCommand('load',{handler:()=>pi.registerTool({name:'late',label:'Late',description:'Late',parameters:{type:'object'},defaultActive:false,
    async execute(){return {content:[{type:'text',text:'late executed'}],details:undefined}}})}); };`);
  const peer = launch(t, [entry], { auto: false }); await peer.init(['dynamic_tools']);
  const command = peer.command('load');
  const register = await peer.wait(frame => frame.method === 'tools/register');
  assert.equal(register.params.tools[0].default_active, false);
  peer.send({ jsonrpc: '2.0', id: register.id, result: { revision: 1, tools: ['late'] } });
  assert.ok((await command.response).result);
  const call = await peer.request('tool/call', { name: 'late', arguments: {}, context: peer.context() }).response;
  assert.equal(call.result.content[0].text, 'late executed'); await peer.close();
});

test('adapter: text/image partial snapshots retain details, explicit null, order and callback-time values', async t => {
  const data = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a0wAAAABJRU5ErkJggg==';
  const entry = await fixture(t, `export default pi => pi.registerTool({name:'probe',label:'Probe',description:'Probe',parameters:{type:'object'},
    async execute(id,args,signal,update){
      const first={content:[{type:'image',mimeType:'image/png',data:'${data}'}],details:{step:1},structuredContent:null};
      update(first); first.details.step=999;
      update({content:[{type:'text',text:'second'}],details:{step:2}});
      return {content:[{type:'text',text:'final'}],details:undefined};
    }});`);
  const peer = launch(t, [entry], { auto: false }); await peer.init(['request_progress', 'artifacts']);
  const call = peer.request('tool/call', { name: 'probe', arguments: {}, context: peer.context() });
  const artifact = await peer.wait(frame => frame.method === 'artifact/publish');
  assert.equal(artifact.params.parent_request_id, call.id);
  assert.equal(artifact.params.sha256, createHash('sha256').update(Buffer.from(data, 'base64')).digest('hex'));
  peer.send({ jsonrpc: '2.0', id: artifact.id, result: { artifact_id: 'host-verified-image' } });
  const first = await peer.wait(frame => frame.method === '$/progress');
  const second = await peer.wait(frame => frame.method === '$/progress');
  assert.equal(first.params.sequence, 1); assert.equal(second.params.sequence, 2);
  assert.equal(first.params.event.type, 'partial_result');
  assert.equal(first.params.event.result.content[0].artifact_id, 'host-verified-image');
  assert.deepEqual(first.params.event.result.metadata.pi_details, { step: 1 });
  assert.equal(first.params.event.result.structured_content, null);
  assert.deepEqual(second.params.event.result.metadata.pi_details, { step: 2 });
  assert.equal(second.params.event.result.content[0].text, 'second');
  assert.equal((await call.response).result.content[0].text, 'final'); await peer.close();
});

test('adapter: actual transport worker admits synchronous frozen catalog and nested live callbacks', async t => {
  const entry = await fixture(t, `export default pi => pi.registerTool({name:'probe',label:'Probe',description:'Probe',parameters:{type:'object'},
    async execute(id,args,signal,update,ctx){
      const tools=ctx.tools;
      if(tools.length!==1||tools[0].name!=='read')throw Error('wrong frozen catalog');
      const updates=[];
      const outcome=await ctx.executeTool('read',{path:'actual'}, {onUpdate:result=>updates.push(result)});
      if(updates.length!==1||updates[0].details.step!==1||updates[0].content[0].data!=='iVBORw==')throw Error('partial callback lost');
      if(outcome.toolCall.id!=='host-outer/1'||outcome.result.details.answer!==42)throw Error('native outcome lost');
      return {content:[{type:'text',text:JSON.stringify(outcome)}],details:{updates}};
    }});`);
  const peer = launch(t, [entry], { auto: false }); await peer.init(['tool_composition_v1', 'request_progress']);
  const call = peer.request('tool/call', { name: 'probe', arguments: {}, context: peer.context() });
  const catalog = await peer.wait(frame => frame.method === 'composition/context');
  assert.equal(catalog.params.parent_request_id, call.id);
  peer.send({ jsonrpc: '2.0', id: catalog.id, result: { tools: [{ name: 'read', description: 'Native read', parameters: { type: 'object' } }] } });
  const nested = await peer.wait(frame => frame.method === 'composition/call');
  assert.equal(nested.params.full_outcome, true); assert.equal(nested.params.updates, true);
  const progress = { request_id: nested.id, sequence: 1, result: { content: [{ Media: { Image: { source: { Inline: 'iVBORw==' }, media_type: 'image/png' } } }], metadata: { pi_details: { step: 1 } }, is_error: false } };
  peer.notify('composition/update', progress);
  peer.notify('composition/update', progress); // Non-monotonic progress is ignored.
  peer.send({ jsonrpc: '2.0', id: nested.id, result: { value: { tool_call: { id: 'host-outer/1', name: 'read', arguments: { path: 'actual' } },
    content: [{ Text: 'actual read' }], metadata: { pi_details: { answer: 42 } }, is_error: false } } });
  const response = await call.response; assert.ok(response.result, JSON.stringify(response));
  assert.equal(response.result.metadata.pi_details.updates.length, 1);
  const outcome = JSON.parse(response.result.content[0].text);
  assert.equal(Object.hasOwn(outcome.result, 'structuredContent'), false);
  peer.notify('composition/update', { ...progress, sequence: 2 }); // Settled reverse calls cannot invoke a callback.
  await peer.close();
});

test('adapter: synchronous frozen catalog sidecars are verified and unlinked', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-tool-context-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const bytes = Buffer.from(JSON.stringify({ tools: [{ name: 'read', description: 'Verified native catalog', parameters: { type: 'object' } }] }));
  await writeFile(join(dir, 'composition-catalog.json'), bytes);
  const entry = await fixture(t, `export default pi => pi.registerTool({name:'probe',label:'Probe',description:'Probe',parameters:{type:'object'},
    async execute(id,args,signal,update,ctx){return {content:[{type:'text',text:ctx.tools[0].description}],details:undefined}}});`);
  const peer = launch(t, [entry], { auto: false, env: { OCTET_EXTENSION_SCRATCH: dir } }); await peer.init(['tool_composition_v1']);
  const call = peer.request('tool/call', { name: 'probe', arguments: {}, context: peer.context() });
  const catalog = await peer.wait(frame => frame.method === 'composition/context');
  peer.send({ jsonrpc: '2.0', id: catalog.id, result: { context_file: { path: 'composition-catalog.json', bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') } } });
  assert.equal((await call.response).result.content[0].text, 'Verified native catalog');
  const { access } = await import('node:fs/promises'); await assert.rejects(access(join(dir, 'composition-catalog.json')), { code: 'ENOENT' });
  await peer.close();
});

test('adapter: cancelled nested calls ignore late partial results without inventing outcomes', async t => {
  const entry = await fixture(t, `export default pi => pi.registerTool({name:'probe',label:'Probe',description:'Probe',parameters:{type:'object'},
    async execute(id,args,signal,update,ctx){
      const controller=new AbortController(); let called=false;
      const pending=ctx.executeTool('read',{}, {signal:controller.signal,onUpdate:()=>{called=true}});
      controller.abort();
      try {await pending;throw Error('cancel should reject')}catch(error){if(error.code!==-32800)throw error}
      await new Promise(resolve=>setTimeout(resolve,30));
      if(called)throw Error('late callback ran');
      return {content:[],details:undefined};
    }});`);
  const peer = launch(t, [entry], { auto: false }); await peer.init(['tool_composition_v1']);
  const call = peer.request('tool/call', { name: 'probe', arguments: {}, context: peer.context() });
  const nested = await peer.wait(frame => frame.method === 'composition/call');
  await peer.wait(frame => frame.method === '$/cancelRequest' && frame.params.id === nested.id);
  peer.notify('composition/update', { request_id: nested.id, sequence: 1, result: { content: [{ Text: 'late' }], is_error: false } });
  peer.send({ jsonrpc: '2.0', id: nested.id, result: { value: { tool_call: { id: 'native/1', name: 'read', arguments: {} }, content: [], is_error: false } } });
  const response = await call.response; assert.ok(response.result, JSON.stringify(response)); await peer.close();
});

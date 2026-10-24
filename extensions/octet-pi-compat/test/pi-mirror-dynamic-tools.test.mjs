// Synthetic reviewed mirrors only; no user factories, credentials or model calls.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureMirror } from '../configure.mjs';
import { Runtime } from '../lib/runtime.mjs';
import { host, launch, owner } from './helper.mjs';

const features = ['dynamic_tools', 'runtime_commands', 'resource_paths_v1', 'session_entries'];
const definition = (name, description = name) => `{name:${JSON.stringify(name)},description:${JSON.stringify(description)},parameters:{type:'object'},execute(){return {content:[{type:'text',text:${JSON.stringify(description)}}]};}}`;
async function fixture(t, source) {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), 'octet-mirror-dynamic-')));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const agent = join(directory, 'agent'), entry = join(agent, 'extensions/fixture.js');
  mkdirSync(join(agent, 'extensions'), { recursive: true });
  writeFileSync(entry, source); writeFileSync(join(agent, 'settings.json'), '{}');
  const output = join(directory, 'octet-pi-compat');
  const captured = await configureMirror({ reviewed: true, output, cwd: directory,
    env: { ...process.env, HOME: directory, OCTET_PI_AGENT_DIR: agent }, log() {} });
  return { directory, output, ...captured };
}
function params(f, offered = features, grants = []) {
  const m = f.registrations;
  return { api_version: '0.4', workspace: f.directory, host: { ...host, has_ui: false },
    capabilities: { builtin_tool_overrides: grants },
    contributes: { tools: m.tools.map(t => t.name), commands: m.commands.map(c => c.name), hooks: m.hooks,
      shortcuts: m.shortcuts, tool_renderers: m.tool_renderers },
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'],
      optional_features: offered, limits: { max_concurrent_requests: 8 } } };
}
async function liveRuntime(t, source, { offered = features, grants = [], requestSync } = {}) {
  const f = await fixture(t, source), calls = [];
  const runtime = new Runtime(f.config, { requestSync(...args) { calls.push(args); return requestSync?.(...args); } });
  t.after(() => { runtime.timers.all(); runtime.uninstallChildren(); });
  await runtime.initialize(params(f, offered, grants), { id: 1 });
  const store = { id: 2, method: 'hook/run', live: true, pending: new Set(), errors: [], controller: new AbortController() };
  runtime.bind({ hook: 'session_start', context: { workspace: f.directory, resource_owner: owner, host: { has_ui: false } } }, store);
  const entry = runtime.events.get('session_start')[0];
  const register = () => runtime.scope.run({ ...store, factory: entry.factory }, () => entry.handler({}, {}));
  return { runtime, store, calls, register };
}

test('mirror session_start registers a late tool only after the native dynamic catalog ACK', async t => {
  const f = await fixture(t, `export default pi => { let accepted=false;
    pi.on('session_start',()=>{pi.registerTool(${definition('subagent_supervisor')});accepted=true;});
    pi.registerCommand('check',{handler(){if(!accepted)throw Error('registration did not finish');}});
  };`);
  assert.deepEqual(f.registrations.tools, [], 'capture does not execute session_start or widen the static catalog');
  const p = launch(t, [], { config: join(f.output, 'bridge.json'), metadata: f.registrations, cwd: f.directory, auto: false });
  assert.ok((await p.request('initialize', params(f)).response).result);
  const start = p.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: p.context({ has_ui: false }) });
  const registration = await Promise.race([
    p.wait(frame => frame.method === 'tools/register'),
    start.response,
  ]);
  assert.equal(registration.method, 'tools/register', `session_start settled without native tool admission: ${JSON.stringify(registration)}; ${p.stderr()}`);
  assert.equal(registration.params.tools[0].name, 'subagent_supervisor');
  assert.equal(registration.params.parent_request_id, undefined, 'catalog mutation is process-owned');
  assert.equal(p.seen.some(frame => frame.id === start.id && !frame.method), false, 'synchronous Pi registration cannot settle before ACK');
  p.send({ jsonrpc: '2.0', id: registration.id, result: { revision: 1, tools: ['subagent_supervisor'] } });
  assert.ok((await start.response).result);
  assert.ok((await p.command('check', [], { has_ui: false }).response).result);
  const call = await p.request('tool/call', { name: 'subagent_supervisor', arguments: {}, context: p.context() }).response;
  assert.equal(call.result.content[0].text, 'subagent_supervisor');
  await p.close();
});

test('mirror dynamic replacements do not publish optimistically and host failure preserves the accepted handler', async t => {
  let runtime, fail = false;
  const live = await liveRuntime(t, `export default pi => {
    pi.registerTool(${definition('probe', 'original')});
    pi.on('session_start',()=>pi.registerTool(${definition('probe', 'replacement')}));
  };`, { requestSync(method, request) {
    assert.equal(method, 'tools/register'); assert.equal(request.tools[0].description, 'replacement');
    assert.equal(runtime.tools.get('probe').definition.description, 'original', 'old handler survives until ACK');
    if (fail) throw Object.assign(Error('native collision'), { code: -32602 });
    return { revision: 1, tools: ['probe'] };
  } });
  runtime = live.runtime;
  const original = runtime.tools.get('probe'); fail = true;
  assert.throws(live.register, /native collision/); assert.equal(runtime.tools.get('probe'), original);
  fail = false; assert.equal(live.register(), undefined);
  assert.equal(runtime.tools.get('probe').definition.description, 'replacement');
});

test('mirror omitted or malformed catalog ACK leaves a new tool unpublished', async t => {
  let reply;
  const live = await liveRuntime(t, `export default pi => pi.on('session_start',()=>pi.registerTool(${definition('late')}));`, { requestSync() { return reply; } });
  for (reply of [{ revision: 1, tools: [] }, { revision: -1, tools: ['late'] }, { revision: '1', tools: ['late'] }, { revision: 1, tools: ['late'], extra: true }]) {
    assert.throws(live.register, /host did not publish|unsupported_feature/);
    assert.equal(live.runtime.tools.has('late'), false);
  }
});

test('mirror late tools require negotiated dynamic_tools before any native mutation', async t => {
  const live = await liveRuntime(t, `export default pi => pi.on('session_start',()=>pi.registerTool(${definition('late')}));`, { offered: features.filter(f => f !== 'dynamic_tools') });
  assert.throws(live.register, /unsupported_feature dynamic_tools/);
  assert.equal(live.calls.length, 0); assert.equal(live.runtime.tools.has('late'), false);
});

test('mirror late builtin tools retain exact native grants and host refusal rollback', async t => {
  const source = `export default pi => { pi.registerTool(${definition('read', 'original')});
    pi.on('session_start',(_event,ctx)=>pi.registerTool(${definition('read', 'replacement')}));
    pi.on('session_end',()=>pi.registerTool(${definition('write')}));
  };`;
  let fail = true;
  const live = await liveRuntime(t, source, { offered: [...features, 'builtin_tool_overrides_v1'], grants: ['read'],
    requestSync() { if(fail)throw Error('native refused replacement'); return { revision: 1, tools: ['read'] }; } });
  const original = live.runtime.tools.get('read');
  assert.throws(live.register, /native refused replacement/); assert.equal(live.runtime.tools.get('read'), original);
  const unreviewed = live.runtime.events.get('session_end')[0];
  assert.throws(() => live.runtime.scope.run({ ...live.store, factory: unreviewed.factory }, () => unreviewed.handler()), /write.*not reviewed/);
  assert.equal(live.calls.length, 1); assert.equal(live.runtime.tools.has('write'), false);
  fail = false; live.register(); assert.equal(live.runtime.tools.get('read').definition.description, 'replacement');
});

test('mirror late builtin without any native grant is refused before catalog submission', async t => {
  const live = await liveRuntime(t, `export default pi => pi.on('session_start',()=>pi.registerTool(${definition('read')}));`, { offered: [...features, 'builtin_tool_overrides_v1'] });
  assert.throws(live.register, /builtin_tool_overrides_v1/); assert.equal(live.calls.length, 0);
});

test('mirror late tool catalog bound runs before native submission', async t => {
  const live = await liveRuntime(t, `export default pi => pi.on('session_start',()=>pi.registerTool(${definition('late')}));`);
  for (let i=0;i<256;i++) live.runtime.tools.set(`existing_${i}`, { definition: {}, factory: 0 });
  assert.throws(live.register, /tool registration limit/); assert.equal(live.calls.length, 0);
});

test('mirror late tool schema validation runs before native submission', async t => {
  const bad = await liveRuntime(t, `export default pi => pi.on('session_start',()=>pi.registerTool({...${definition('late')},parameters:{type:'object',unsupported:true}}));`);
  assert.throws(bad.register, /JSON Schema keyword/); assert.equal(bad.calls.length, 0);
});

test('mirror late tools cannot use missing, retired or inactive owners or a failed factory', async t => {
  const live = await liveRuntime(t, `export default pi => pi.on('session_start',()=>pi.registerTool(${definition('late')}));`);
  const entry = live.runtime.events.get('session_start')[0];
  assert.throws(() => entry.handler({}, {}), /not_foreground_owner/);
  live.store.state.alive = false; assert.throws(live.register, /not_foreground_owner/);
  live.store.state.alive = true; live.runtime.foreground = {};
  assert.throws(live.register, /not_foreground_owner/);
  live.runtime.foreground = live.store.state;
  for (const foreign of [{ ...owner, process_generation: owner.process_generation + 1 }, { ...owner, extension_instance_id: 'foreign-instance' }]) {
    assert.throws(() => live.runtime.bind({ context: { resource_owner: foreign } }, { ...live.store, method: 'command/execute' }), /not_foreground_owner process binding/);
  }
  live.runtime.failedFactories.add(entry.factory);
  assert.throws(live.register, /failed extension factory/);
  assert.equal(live.calls.length, 0);
});

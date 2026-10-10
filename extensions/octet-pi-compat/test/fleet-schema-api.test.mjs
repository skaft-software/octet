import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createAPI } from '../lib/api.mjs';
import { facade, strict } from '../lib/errors.mjs';
import { validateTool, toolWire } from '../lib/tools.mjs';
import { Compile } from '../node_modules/typebox/build/compile/index.mjs';
import { inspect, launch } from './helper.mjs';

// Shared with native argument-validation tests, not a JS-only acceptance profile.
const cases = JSON.parse(readFileSync(new URL('../../../crates/octet-ai/src/json_repair/pattern_cases.json', import.meta.url), 'utf8'));
const definition = parameters => ({ name: 'fleet_probe', description: 'Fleet schema probe', parameters, execute() {} });
const schema = pattern => ({ type: 'object', properties: { key: { type: 'string', pattern } }, required: ['key'] });

function api() {
  return createAPI({ assertFactory() {}, config: { extensions: ['/reviewed/probe.ts'] }, transcript: { api: () => ({}) }, bus: { facade: () => ({}) } }, 0);
}

test('Pi 1.0.2 absent API members support optional feature detection, not invented success', () => {
  const pi = api();
  for (const name of ['unregisterTool', 'futurePrivateMethod']) {
    assert.equal(pi[name], undefined);
    assert.equal(name in pi, false);
    assert.equal(Object.hasOwn(pi, name), false);
    assert.equal(pi[name]?.('stale'), undefined);
    assert.throws(() => pi[name]('stale'), TypeError);
  }
  assert.equal(Object.getPrototypeOf(pi), Object.prototype);
  // Pi's own callback objects are plain: an optional member a factory probes
  // (`ctx.goalStorageRoot`, `pi.events[channel]`, an event field from another
  // release) is absent, not an error, and assigning an undefined member cannot
  // silently shadow the facade. An octet-built data object keeps refusing an
  // unavailable fact, because there absence means the host could not supply it.
  assert.equal(facade({}, 'ctx').missing, undefined);
  assert.equal('missing' in facade({}, 'ctx'), false);
  assert.equal(facade({}, 'ctx').missing?.('stale'), undefined);
  assert.throws(() => { facade({}, 'ctx').missing = 1; }, /unsupported_feature ctx.missing/);
  assert.throws(() => strict({}, 'assistant usage').missing, /unsupported_feature assistant usage.missing/);
});

test('known unimplemented Pi API members remain explicit refusing methods', () => {
  const pi = api();
  for (const name of ['registerVirtualModel', 'unregisterVirtualModel']) {
    assert.equal(typeof pi[name], 'function');
    assert.throws(() => pi[name]({}), new RegExp(`unsupported_feature pi\\.${name}`));
  }
});

test('portable bounded pattern profile preserves schemas and agrees with pinned TypeBox', () => {
  for (const entry of cases) {
    const parameters = schema(entry.pattern), before = JSON.stringify(parameters);
    if (entry.accepted) {
      validateTool(definition(parameters));
      assert.deepEqual(toolWire('fleet_probe', { definition: definition(parameters) }).parameters, parameters);
      const oracle = Compile(parameters);
      for (const [key, matches] of entry.values) assert.equal(oracle.Check({ key }), matches, `${entry.pattern}: ${JSON.stringify(key)}`);
    } else {
      assert.throws(() => validateTool(definition(parameters)), /pattern.*not supported/, entry.pattern);
    }
    assert.equal(JSON.stringify(parameters), before, 'no constraint is stripped or rewritten');
  }
});

test('pattern admission validates types and bounds without weakening other constraints', () => {
  for (const pattern of [null, 3, {}, true]) assert.throws(() => validateTool(definition(schema(pattern))), /pattern/);
  assert.throws(() => validateTool(definition(schema('^' + 'a'.repeat(1023) + '$'))), /pattern/);
  assert.throws(() => validateTool(definition({ type: 'string', format: 'email' })), /format.*not supported/);
  const parameters = schema('^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$');
  const oracle = Compile(parameters);
  assert.equal(oracle.Check({ key: 'a'.repeat(128) }), true);
  assert.equal(oracle.Check({ key: 'a'.repeat(129) }), false);
  validateTool(definition(schema('^a{1024}$')));
});

test('deprecated is a boolean annotation, retained inside unions without changing acceptance', () => {
  const parameters = { type: 'object', properties: { acceptance: { anyOf: [
    { type: 'string', enum: ['checked'] }, { type: 'string', enum: ['reviewed'], deprecated: true },
  ] } } };
  validateTool(definition(parameters));
  assert.deepEqual(toolWire('fleet_probe', { definition: definition(parameters) }).parameters, parameters);
  const oracle = Compile(parameters);
  assert.equal(oracle.Check({ acceptance: 'reviewed' }), true);
  assert.equal(oracle.Check({ acceptance: 'other' }), false);
  for (const deprecated of [null, 'yes', 0, {}]) assert.throws(() => validateTool(definition({ type: 'object', deprecated })), /deprecated.*boolean/);
});

test('real registration capture accepts MCP optional probe and bounded fleet schemas', t => {
  const dir = mkdtempSync(join(tmpdir(), 'octet-fleet-schema-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const factory = join(dir, 'probe.ts');
  const parameters = { ...schema('^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$'), deprecated: false };
  writeFileSync(factory, `export default pi => {
    const unregisterTool = pi.unregisterTool;
    if (unregisterTool !== undefined || 'unregisterTool' in pi) throw new Error('not Pi absence');
    for (const stale of []) unregisterTool?.(stale);
    pi.registerTool({name:'fleet_probe',description:'Fleet probe',parameters:${JSON.stringify(parameters)},execute(){return {content:[]};}});
  };`);
  assert.deepEqual(inspect([factory]).tools[0].parameters, parameters);
});

test('optional MCP removal probing does not bypass dynamic registration ACK or refusal', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'octet-fleet-register-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const factory = join(dir, 'probe.ts');
  writeFileSync(factory, `export default pi => {
    const unregisterTool = pi.unregisterTool;
    let revision = 0;
    pi.registerCommand('load', {handler() {
      unregisterTool?.('late');
      const label = String(++revision);
      pi.registerTool({name:'late',description:label,parameters:{type:'object'},
        execute(){ return {content:[{type:'text',text:label}]}; }});
    }});
  };`);
  const peer = launch(t, [factory], { auto: false });
  await peer.init(['dynamic_tools']);
  const first = peer.command('load');
  const accepted = await peer.wait(f => f.method === 'tools/register');
  peer.send({ jsonrpc: '2.0', id: accepted.id, result: { revision: 1, tools: ['late'] } });
  assert.ok((await first.response).result);
  const second = peer.command('load');
  const refused = await peer.wait(f => f.method === 'tools/register');
  peer.send({ jsonrpc: '2.0', id: refused.id, error: { code: -32602, message: 'host refused replacement' } });
  assert.match((await second.response).error.message, /host refused replacement/);
  const result = await peer.request('tool/call', { name: 'late', arguments: {}, context: peer.context() }).response;
  assert.equal(result.result.content[0].text, '1', 'refused replacement did not mutate the accepted tool');
  assert.equal(peer.seen.some(f => f.method === 'tools/unregister'), false);
  await peer.close();
});

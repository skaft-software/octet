import test from 'node:test';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {Extension} from '../process/index.mjs';
import {harness, initialize, request, tool} from './harness.mjs';

const source = fileURLToPath(new URL('fixtures/typed-author.mjs', import.meta.url));
const setup = async t => {
  const h = harness(t, {source});
  const offer = initialize(); offer.contributes.tools = ['typed_stats', 'typed_null']; offer.contributes.commands = [];
  return {h, catalog: await h.ready(offer)};
};
const call = (id, arguments_ = {}) => tool(id, arguments_, {name: 'typed_stats'});

test('typed SDK process declares exact schema and returns structured values plus explicit text', {timeout: 10000}, async t => {
  const {h, catalog} = await setup(t);
  assert.deepEqual(catalog.tools[0].output_schema, {type: 'object', properties: {
    characters: {type: 'integer', minimum: 0}, note: {type: 'null'},
  }, required: ['characters', 'note'], additionalProperties: false});
  h.send(call(2, {text: 'hé 🦀'}));
  assert.deepEqual((await h.reply(2)).result, {content: [{type: 'text', text: '4 Unicode characters'}],
    is_error: false, structured_content: {characters: 4, note: null}});
  h.send(tool(3, {}, {name: 'typed_null'}));
  assert.deepEqual((await h.reply(3)).result.structured_content, null);
  assert(Object.hasOwn((await h.reply(3)).result, 'structured_content'));
  await h.stop();
});

test('typed output failures refuse publication and preserve a healthy next call', {timeout: 10000}, async t => {
  const {h} = await setup(t);
  for (const [id, mode] of [[2, 'invalid'], [3, 'nonfinite'], [4, 'extra']]) {
    h.send(call(id, {mode}));
    assert.equal((await h.reply(id)).error.code, -32603);
    assert(!Object.hasOwn(await h.reply(id), 'result'));
  }
  h.send(call(5, {mode: 'error'}));
  assert.deepEqual((await h.reply(5)).result, {content: [{type: 'text', text: 'Expected domain failure'}], is_error: true});
  h.send(call(6, {text: 'ok'}));
  assert.equal((await h.reply(6)).result.structured_content.characters, 2);
  await h.stop();
});

test('typed cancellation has one terminal and no successful typed result', {timeout: 10000}, async t => {
  const {h} = await setup(t);
  h.send(call(2, {mode: 'cancel'})); await h.progress(2);
  h.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id: 2}});
  assert.equal((await h.reply(2)).error.code, -32800);
  h.send(call(3, {text: 'next'}));
  assert.equal((await h.reply(3)).result.structured_content.characters, 4);
  await h.stop();
  assert.equal(h.frames.filter(frame => frame.id === 2).length, 1);
});

test('typed registration requires an output schema and explicit projection', () => {
  const definition = {name: 'typed', description: 'Typed', parameters: {type: 'object'}};
  assert.throws(() => new Extension().typedTool(definition, () => 1, String), /outputSchema/);
  assert.throws(() => new Extension().typedTool({...definition, outputSchema: {type: 'integer'}}, () => 1), /projection/);
  assert.throws(() => new Extension().typedTool({...definition, outputSchema: {type: 'string', pattern: '.*'}}, () => 'a', String), /Unsupported schema/);
});

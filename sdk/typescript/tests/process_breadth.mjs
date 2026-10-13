import test from 'node:test';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {Extension, resourceType, validateDiagnostics, diagnosticSummary, validateBlob} from '../process/index.mjs';
import {harness, initialize, request, tool, context} from './harness.mjs';

const source = fileURLToPath(new URL('fixtures/breadth-author.mjs', import.meta.url));
export const breadthOffer = () => {
  const offer = initialize();
  offer.contributes.tools = ['create', 'increment', 'release', 'service', 'blob']; offer.contributes.commands = [];
  offer.contributes.hooks = ['before_prompt', 'cache_warming_decision', 'model_turn_start'];
  offer.protocol.optional_features.push('artifacts', 'composer', 'resource_refs_v1', 'operation_descriptors_v1', 'bulk_objects_v1', 'cache_warming_decision', 'session_entries');
  offer.protocol.limits.resource_refs_v1 = {max_records: 256, max_registrations_per_parent: 32};
  offer.protocol.bulk_objects_v1 = {profile: 'local-file.v1', transfer_directory: '/private/host-owned-transfer', limits: {
    object_bytes: 268435456, owner_bytes: 536870912, write_tickets_per_generation: 8, read_leases_per_generation: 32, blobs_per_owner: 256,
  }};
  return offer;
};
const setup = async t => { const h = harness(t, {source}); const catalog = await h.ready(breadthOffer()); return {h, catalog}; };
const call = (id, name, args = {}, ctx = context) => tool(id, args, {name, context: ctx});
const reverse = (h, method, parent) => h.wait(f => f.method === method && f.params.parent_request_id === parent);
const answer = (h, child, result) => h.send({jsonrpc: '2.0', id: child.id, result});
const ref = {$resource: 'host:counter:1', type: 'example.Counter'};
const create = async (h, id = 2) => {
  h.send(call(id, 'create')); const child = await reverse(h, 'resource/register', id);
  assert.deepEqual(child.params, {type: ref.type, parent_request_id: id}); answer(h, child, ref);
  assert.deepEqual((await h.reply(id)).result.structured_content, {counter: ref});
};

test('resource schema generates slots; native identity persists and disposal invalidates before reuse', {timeout: 10000}, async t => {
  const {h, catalog} = await setup(t);
  assert.equal(catalog.protocol.limits.max_concurrent_requests, 1);
  assert.deepEqual(catalog.tools[1].operation, {id: 'increment', receiver: '/counter', resource_inputs: [{path: '/counter', type: ref.type, access: 'exclusive'}], resource_outputs: []});
  await create(h);
  h.send(call(3, 'increment', {counter: ref})); assert.equal((await h.reply(3)).result.structured_content, 1);
  h.send(call(4, 'increment', {counter: ref}, {...context, resource_owner: {...context.resource_owner, session_id: 'foreign'}}));
  assert.equal((await h.reply(4)).error.code, -32602);
  h.send(call(5, 'release')); const release = await reverse(h, 'resource/release', 5);
  assert.deepEqual(release.params.resource, ref); answer(h, release, {retired: true, cleanup: 'pending'}); await h.reply(5);
  h.send(request(6, 'resource/dispose', {resources: [ref], reason: 'retired'}));
  assert.deepEqual((await h.reply(6)).result, {results: [{resource: ref, status: 'completed'}]});
  h.send(call(7, 'increment', {counter: ref})); assert.equal((await h.reply(7)).error.code, -32602);
  h.send(request(8, 'resource/dispose', {resources: [ref], reason: 'retired'}));
  assert.equal((await h.reply(8)).result.results[0].status, 'failed');
  await h.stop(); assert.equal(h.stderr().match(/increment entered/g)?.length, 1); assert.match(h.stderr(), /disposed:1/);
});

test('late registration after retirement cannot resurrect local native state', {timeout: 10000}, async t => {
  const {h} = await setup(t); h.send(call(2, 'create'));
  const child = await reverse(h, 'resource/register', 2);
  h.send(request(3, 'resource/dispose', {resources: [ref], reason: 'retired'}));
  answer(h, child, ref);
  assert.equal((await h.reply(2)).error.code, -32603);
  assert.equal((await h.reply(3)).result.results[0].status, 'failed');
  h.send(call(4, 'increment', {counter: ref})); assert.equal((await h.reply(4)).error.code, -32602);
  await h.stop(); assert(!h.stderr().includes('increment entered'));
});

test('hooks dispatch with actual payload/context, negotiate features and preserve private append correlation', {timeout: 10000}, async t => {
  const {h} = await setup(t);
  h.send(request(2, 'hook/run', {hook: 'before_prompt', payload: {prompt: 'hello'}, context}));
  assert.equal((await h.reply(2)).result.context[0].content, 'hello');
  h.send(request(3, 'hook/run', {hook: 'cache_warming_decision', payload: {}, context}));
  assert.equal((await h.reply(3)).result.cache_warming_decision, 'stop');
  const grant = {grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'op:1', owner: context.resource_owner, expected_head: null};
  h.send(request(4, 'hook/run', {hook: 'model_turn_start', payload: {append: true}, context, session_leaf: grant}));
  const child = await reverse(h, 'session/append_entry', 4);
  assert.deepEqual(child.params.session_leaf, {grant_id: grant.grant_id, activation_epoch: 4, operation_id: 'op:1'});
  answer(h, child, {entry_id: 'actual-entry', head: 'actual-entry', successor: null});
  assert.equal((await h.reply(4)).result.session_operation.action, 'continue');
  h.send(request(5, 'hook/run', {hook: 'model_turn_start', payload: {bad: true}, context}));
  assert.equal((await h.reply(5)).error.code, -32603);
  await h.stop(); assert(!h.stderr().includes(grant.grant_id));
});

test('private hook appends consume and advance only authenticated successor grants', {timeout: 10000}, async t => {
  const {h} = await setup(t);
  const grant = {grant_id: 'a'.repeat(64), activation_epoch: 4, operation_id: 'op:1', owner: context.resource_owner, expected_head: null};
  for (const [id, count] of [[2, 2], [3, 3]]) {
    h.send(request(id, 'hook/run', {hook: 'model_turn_start', payload: {append: count}, context, session_leaf: grant}));
    const first = await reverse(h, 'session/append_entry', id);
    const successor = {...grant, grant_id: 'b'.repeat(64), expected_head: 'entry-1'};
    answer(h, first, {entry_id: 'entry-1', head: 'entry-1', successor});
    const second = await h.wait(f => f.method === 'session/append_entry' && f.params.parent_request_id === id && f.id !== first.id);
    assert.equal(second.params.session_leaf.grant_id, successor.grant_id);
    answer(h, second, {entry_id: 'entry-2', head: 'entry-2', successor: null});
    const reply = await h.reply(id);
    if (count === 2) assert.equal(reply.result.session_operation.action, 'continue');
    else assert.equal(reply.error.code, -32603);
    assert.equal(h.frames.filter(f => f.method === 'session/append_entry' && f.params.parent_request_id === id).length, 2);
  }
  await h.stop();
});

test('diagnostics and verified-host artifact publication produce native content parts', {timeout: 10000}, async t => {
  const {h} = await setup(t);
  h.send(call(2, 'service', {mode: 'diagnostic'})); const result = (await h.reply(2)).result;
  assert.equal(result.metadata.octet_diagnostics_v1[0].code, 'solver.failed');
  assert.deepEqual(result.content[1], {type: 'text', text: 'error[solver.failed]: Failed now'});
  h.send(call(3, 'service', {mode: 'invalid-diagnostic'})); assert.equal((await h.reply(3)).error.code, -32603);
  h.send(call(4, 'service', {mode: 'media'})); const publish = await reverse(h, 'artifact/publish', 4);
  assert.equal(publish.params.size, 3); assert.equal(publish.params.data.data, 'AQID');
  assert.equal(publish.params.sha256, createHash('sha256').update(new Uint8Array([1, 2, 3])).digest('hex'));
  answer(h, publish, {artifact_id: 'host:artifact'});
  assert.deepEqual((await h.reply(4)).result.content[1], {type: 'image', artifact_id: 'host:artifact', mime_type: 'image/png', alt: 'Preview'});
  await h.stop();
});

test('reverse calls settle errors, cancel with parent, ignore late replies, reject stale/forged authority', {timeout: 10000}, async t => {
  const {h} = await setup(t);
  h.send(call(2, 'service', {mode: 'get'})); const first = await reverse(h, 'composer/get', 2);
  h.send({jsonrpc: '2.0', id: first.id, error: {code: -32002, message: 'not_foreground_owner'}});
  assert.equal((await h.reply(2)).result.content[0].text, 'Host error -32002');
  h.send(call(3, 'service', {mode: 'get'})); const cancelled = await reverse(h, 'composer/get', 3);
  h.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id: 3}});
  assert.equal((await h.reply(3)).error.code, -32800);
  await h.wait(f => f.method === '$/cancelRequest' && f.params.id === cancelled.id);
  answer(h, cancelled, {text: 'late'});
  h.send(call(4, 'service', {mode: 'stale'})); assert.equal((await h.reply(4)).error.code, -32800);
  h.send(call(5, 'service', {mode: 'forged'})); assert.equal((await h.reply(5)).error.code, -32603);
  h.send(call(6, 'service', {mode: 'get'})); const next = await reverse(h, 'composer/get', 6);
  assert.notEqual(next.id, first.id); assert.notEqual(next.id, cancelled.id);
  answer(h, next, {text: 'healthy'}); assert.equal((await h.reply(6)).result.content[0].text, '{"text":"healthy"}');
  await h.stop(); assert.equal(h.frames.filter(f => f.id === 3).length, 1);
});

test('low-level bulk write/commit is reachable without embedding bytes or locators in results', {timeout: 10000}, async t => {
  const {h} = await setup(t); h.send(call(2, 'blob'));
  const write = await reverse(h, 'bulk/write', 2); answer(h, write, {ticket: 'ticket:1', profile: 'local-file.v1', locator: 'private-transfer', capacity: 0});
  const commit = await reverse(h, 'bulk/commit', 2);
  const blob = {$blob: 'host:blob', bytes: 0, digest: commit.params.digest, media_type: 'application/octet-stream'};
  answer(h, commit, blob); const result = (await h.reply(2)).result;
  assert.deepEqual(result.structured_content, blob); assert(!JSON.stringify(result).includes('private-transfer'));
  await h.stop();
});

test('required author services and declared hooks fail initialization when unavailable', {timeout: 10000}, async t => {
  const h = harness(t, {source}); const offer = breadthOffer();
  offer.protocol.optional_features = offer.protocol.optional_features.filter(f => f !== 'bulk_objects_v1');
  h.send(request(1, 'initialize', offer)); assert.equal((await h.reply(1)).error.code, -32602);
  h.child.stdin.end(); assert.equal((await h.exited).code, 0);
});

test('diagnostic closed records, revision fixes, portable byte spans and projection bounds', () => {
  const primary = {source: {kind: 'workspace', path: 'src/main.rs', revision: 'a'.repeat(64)}, span: {start_byte: 0, end_byte: 2}};
  const d = {severity: 'warning', code: 'parser.syntax', message: 'Expected value', primary,
    related: [{message: 'Here', location: primary}], fixes: [{title: 'Insert', edits: [{location: primary, replacement: ''}]}], attachments: [{kind: 'blob', id: 'blob:one'}]};
  validateDiagnostics([d]);
  for (const bad of [{...d, primary: null}, {...d, extra: 1}, {...d, attachments: null}, {...d, code: 'bad code'},
    {...d, primary: {...primary, span: {start_byte: 3, end_byte: 2}}},
    {...d, fixes: [{title: 'Bad', edits: [{location: {...primary, source: {kind: 'blob', id: 'x'}}, replacement: 'x'}]}]}]) assert.throws(() => validateDiagnostics([bad]));
  const values = Array.from({length: 9}, () => ({severity: 'info', code: 'x', message: 'é'.repeat(2000)}));
  const summary = diagnosticSummary(values); assert(Buffer.byteLength(summary) <= 4096); assert(!summary.includes('�'));
  assert.throws(() => validateBlob({$blob: 'x', bytes: 0, digest: {algorithm: 'sha256', value: 'A'.repeat(64)}, media_type: 'text/plain'}));
});

test('resource declarations reject array and root slots; ordinary tools have no new requirements', () => {
  const type = resourceType('example.Counter');
  const definition = {name: 'x', description: 'X', parameters: {type: 'object'}, outputSchema: type.schema};
  assert.throws(() => new Extension().tool(definition, () => ''), /fixed object/);
  assert.throws(() => new Extension().tool({...definition, outputSchema: {type: 'array', items: type.schema}}, () => ''), /fixed object/);
  assert.throws(() => new Extension().hook('invented', () => {}));
});

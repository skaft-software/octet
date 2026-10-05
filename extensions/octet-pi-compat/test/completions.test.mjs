import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { root, launch } from './helper.mjs';
import { configure } from '../configure.mjs';
import { commandArgumentRequest } from '../lib/completions.mjs';

const fixture = join(root, 'test/fixtures/completions.ts');
const snapshot = (text, cursor = Buffer.byteLength(text), revision = 1) => ({ text, cursor, revision });
const query = (peer, params) => peer.request('ui/autocomplete/complete', params);
async function state(peer, args = {}) {
  const response = await peer.request('tool/call', { name: 'completion_state', arguments: args, context: peer.context() }).response;
  assert.ok(response.result, JSON.stringify(response)); return JSON.parse(response.result.content[0].text);
}
async function started(t, options = {}) {
  const peer = launch(t, [fixture], options); await peer.init(['autocomplete']); return peer;
}

test('completion registration follows initialize, has no fabricated parent/owner, and waits for real admission', async t => {
  const peer = await started(t, { hold: ['ui/autocomplete/register'] });
  const register = await peer.wait(f => f.method === 'ui/autocomplete/register');
  assert.deepEqual(register.params, { revision: 1 });
  assert.ok(peer.seen.indexOf(register) > peer.seen.findIndex(f => f.id === 1 && f.result));
  assert.deepEqual(peer.metadata.argument_completions, ['complete']);
  const pending = query(peer, snapshot('/complete async'));
  assert.deepEqual((await state(peer)).calls, []);
  assert.equal(peer.seen.some(f => f.id === pending.id && !f.method), false);
  peer.send({ jsonrpc: '2.0', id: register.id, result: { accepted: true } });
  assert.deepEqual((await pending.response).result, { prefix: 'async', items: [{ value: 'async✓', label: '選択', description: 'exact raw prefix' }] });
  await peer.close();
});

test('real callback receives raw arguments, not tokenized words; byte cursor keeps all untouched text', async t => {
  const peer = await started(t);
  const before = 'previous😀line\n\t  /complete  "two words"  前😀';
  const after = 'tail\nnext line';
  const params = snapshot(before + after, Buffer.byteLength(before), 17);
  const response = await query(peer, params).response;
  const prefix = ' "two words"  前😀';
  assert.deepEqual(response.result, { prefix, items: [{ value: `${prefix}✓`, label: '選択', description: 'exact raw prefix' }] });
  assert.deepEqual((await state(peer)).calls, [prefix]);
  // Exact native suffix range, in UTF-8 bytes, preserves leading whitespace,
  // earlier lines and all text after the cursor. This is not a native UI test.
  const bytes = Buffer.from(params.text), result = response.result;
  const applied = Buffer.concat([bytes.subarray(0, params.cursor - Buffer.byteLength(result.prefix)), Buffer.from(result.items[0].value), bytes.subarray(params.cursor)]).toString('utf8');
  assert.equal(applied, before + '✓' + after);
  const empty = await query(peer, snapshot('/complete ')).response;
  assert.equal(empty.result.prefix, ''); assert.equal(empty.result.items[0].value, '✓');
  // Command execution remains array-based; no invented reconstruction from a
  // prior completion snapshot, nor destructive splitting of each supplied arg.
  assert.ok((await peer.command('complete', ['"two  words"', '尾']).response).result);
  await peer.wait(f => f.method === 'notification' && f.params.message === '"two  words" 尾');
  await peer.close();
});

test('non-command/file/attachment input is unclaimed and never calls a command completer', async t => {
  const peer = await started(t);
  for (const text of ['src/ma', '/unknown x', '/plain x', '/complete', 'text /complete x', '/complete\tx', '/complete @file', '/complete (@file', '/complete ，@file', '/complete @"space name']) {
    assert.deepEqual((await query(peer, snapshot(text)).response).result, { prefix: '', items: [] }, text);
  }
  assert.deepEqual((await state(peer)).calls, []);
  // Parent native test must verify this empty response falls through to native
  // file completion ONLY for the still-current text/cursor/revision snapshot.
  await peer.close();
});

test('Pi applyCompletion adds a space when an earlier attachment token starts the full raw argument prefix', async t => {
  const peer = await started(t);
  for (const prefix of ['@first second', '@"two words" next']) {
    const before = `/complete ${prefix}`, after = 'TAIL';
    const reply = await query(peer, snapshot(before + after, Buffer.byteLength(before))).response;
    assert.equal(reply.result.prefix, prefix);
    assert.equal(reply.result.items[0].value, `${prefix}✓ `);
    assert.equal('/complete ' + reply.result.items[0].value + after, before + '✓ ' + after);
  }
  assert.deepEqual((await state(peer)).calls, ['@first second', '@"two words" next']);
  await peer.close();
});

test('Pi null, empty and non-array results are no suggestions; callback exceptions remain errors', async t => {
  const peer = await started(t);
  for (const prefix of ['null', 'empty', 'non-array']) {
    assert.deepEqual((await query(peer, snapshot(`/complete ${prefix}`)).response).result, { prefix: '', items: [] });
  }
  for (const [prefix, message] of [['throw', 'completion callback failed'], ['reject', 'async completion failed']]) {
    const reply = await query(peer, snapshot(`/complete ${prefix}`)).response;
    assert.equal(reply.error.code, -32603); assert.equal(reply.error.message, message);
  }
  assert.ok((await query(peer, snapshot('/complete async')).response).result);
  await peer.close();
});

test('Unicode byte-boundary, bounds, control, revision and unknown-field errors never invoke a callback', async t => {
  const peer = await started(t);
  const text = '/complete 😀';
  for (const cursor of [-1, 0.5, text.length, Buffer.byteLength(text) - 1, 999, null]) {
    assert.match((await query(peer, snapshot(text, cursor)).response).error.message, /UTF-8 byte boundary/);
  }
  for (const revision of [-1, 0.5, null, Number.MAX_SAFE_INTEGER + 1]) {
    assert.match((await query(peer, snapshot(text, Buffer.byteLength(text), revision)).response).error.message, /autocomplete revision/);
  }
  for (const params of [snapshot('/complete \x1b'), snapshot('/complete \u0085'), snapshot('/complete \ud800'),
    snapshot('x'.repeat(262145)), { ...snapshot(text), force: true }, { ...snapshot(text), context: peer.context() }]) {
    assert.ok((await query(peer, params).response).error, JSON.stringify(params).slice(0, 80));
  }
  assert.deepEqual((await state(peer)).calls, []);
  await peer.close();
});

test('unrepresentable result fields and raw tab prefixes fail explicitly without truncation or coercion', async t => {
  const peer = await started(t);
  for (const [prefix, expected] of [['many', /bounds_exceeded autocomplete items/], ['long', /bounds_exceeded autocomplete value/],
    ['control', /unsupported_feature autocomplete value/], ['sparse', /autocomplete item must be an object/], ['bad-item', /autocomplete label must be UTF-8 text/],
    ['unknown-field', /unsupported_feature autocomplete item.effect/], ['a\tb', /unsupported_feature autocomplete prefix/]]) {
    assert.match((await query(peer, snapshot(`/complete ${prefix}`)).response).error.message, expected);
  }
  assert.equal((await state(peer)).calls.at(-1), 'a\tb', 'the callback still receives the exact raw prefix');
  await peer.close();
});

test('exact 32-item and 1024-byte limits succeed while oversized raw prefixes are not truncated', async t => {
  const peer = await started(t);
  const reply = await query(peer, snapshot('/complete maximum')).response;
  assert.equal(reply.result.items.length, 32);
  for (const item of reply.result.items) for (const field of ['value', 'label', 'description']) assert.equal(Buffer.byteLength(item[field]), 1024);
  const prefix = 'p'.repeat(1024);
  assert.equal((await query(peer, snapshot(`/complete ${prefix}`)).response).result.prefix, prefix);
  assert.match((await query(peer, snapshot(`/complete ${prefix}p`)).response).error.message, /bounds_exceeded autocomplete prefix/);
  for (const field of ['label', 'description']) {
    assert.match((await query(peer, snapshot(`/complete control-${field}`)).response).error.message, new RegExp(`unsupported_feature autocomplete ${field}`));
  }
  await peer.close();
});

test('quote-after-cursor consumption and quoted-directory cursor offsets refuse the insufficient native wire', async t => {
  const peer = await started(t);
  assert.match((await query(peer, snapshot('/complete "fo" tail', Buffer.byteLength('/complete "fo'))).response).error.message, /autocomplete replacement range/);
  assert.match((await query(peer, snapshot('/complete "dir')).response).error.message, /autocomplete cursor offset/);
  const ordinary = await query(peer, snapshot('/complete "fo')).response;
  assert.deepEqual(ordinary.result, { prefix: '"fo', items: [{ value: '"folder"', label: 'folder' }] });
  await peer.close();
});

test('out-of-order stale completion results remain correlated to their own immutable snapshots', async t => {
  const peer = await started(t);
  const old = query(peer, snapshot('/complete slow:old', undefined, 10));
  assert.deepEqual((await state(peer, { wait_for: 'slow:old' })).pending, ['slow:old']);
  const current = query(peer, snapshot('/complete fresh', undefined, 11));
  assert.equal((await current.response).result.prefix, 'fresh');
  await state(peer, { release: 'slow:old' });
  const stale = await old.response;
  assert.equal(stale.result.prefix, 'slow:old'); assert.equal(stale.id, old.id);
  assert.ok(peer.seen.findIndex(f => f.id === current.id && !f.method) < peer.seen.findIndex(f => f.id === old.id && !f.method));
  // No global revision high-water mark: revisions may restart for a new editor.
  assert.equal((await query(peer, snapshot('/complete reset', undefined, 0)).response).result.prefix, 'reset');
  // Actual stale display/application rejection belongs to the native full
  // snapshot fence, not an unowned adapter-side guess about session revisions.
  await peer.close();
});

test('cancellation settles a pending callback promptly and a late resolution emits no second terminal reply', async t => {
  const peer = await started(t);
  const pending = query(peer, snapshot('/complete slow:cancel'));
  assert.deepEqual((await state(peer, { wait_for: 'slow:cancel' })).pending, ['slow:cancel']);
  peer.notify('$/cancelRequest', { id: pending.id });
  assert.equal((await pending.response).error.code, -32800);
  await state(peer, { release: 'slow:cancel' });
  await query(peer, snapshot('/complete after')).response;
  assert.equal(peer.seen.filter(f => f.id === pending.id && !f.method).length, 1);
  await peer.close();
});

test('cancelling a request before admission does not cancel the process-scoped registration or later requests', async t => {
  const peer = await started(t, { hold: ['ui/autocomplete/register'] });
  const register = await peer.wait(f => f.method === 'ui/autocomplete/register');
  const pending = query(peer, snapshot('/complete never-invoked'));
  peer.notify('$/cancelRequest', { id: pending.id });
  assert.equal((await pending.response).error.code, -32800);
  peer.send({ jsonrpc: '2.0', id: register.id, result: { accepted: true } });
  assert.equal((await query(peer, snapshot('/complete later')).response).result.prefix, 'later');
  assert.deepEqual((await state(peer)).calls, ['later']);
  assert.equal(peer.seen.some(f => f.method === '$/cancelRequest' && f.params.id === register.id), false);
  await peer.close();
});

for (const accepted of [false, 'true']) test(`refused/malformed autocomplete admission ${JSON.stringify(accepted)} cannot run callbacks`, async t => {
  const peer = await started(t, { completionAccepted: accepted });
  await peer.wait(f => f.method === 'notification' && /autocomplete registration/.test(f.params.message));
  assert.match((await query(peer, snapshot('/complete x')).response).error.message, /autocomplete registration/);
  assert.deepEqual((await state(peer)).calls, []);
  await peer.close();
});

test('completion factories require negotiation and configure retains callback declarations without invalid manifest fields', async t => {
  const peer = launch(t, [fixture]);
  await assert.rejects(peer.init([]), /unsupported_feature autocomplete/);
  assert.equal(peer.seen.some(f => f.method === 'ui/autocomplete/register'), false); await peer.close();
  const dir = await mkdtemp(join(tmpdir(), 'octet-completion-config-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const output = join(dir, 'octet-pi-compat');
  const { registrations } = configure({ reviewed: true, output, extensions: [fixture] });
  assert.deepEqual(registrations.argument_completions, ['complete']);
  assert.doesNotMatch(await readFile(join(output, 'extension.toml'), 'utf8'), /argument_completions|getArgumentCompletions|autocomplete/);
  const configured = launch(t, [fixture], { config: join(output, 'bridge.json') });
  configured.metadata.hooks = registrations.hooks;
  await configured.init(['autocomplete', 'resource_paths_v1', 'session_entries', 'pipeline_hooks_v1']);
  assert.equal((await query(configured, snapshot('/complete configured')).response).result.prefix, 'configured');
  await configured.close();
});

const original = process.env.PI_COMMANDS_PATH;
test('unchanged pinned Pi 1.0.2 commands example loads and its actual completion callback executes', {
  skip: !original && 'set PI_COMMANDS_PATH to the reviewed pinned commands.ts example',
}, async t => {
  assert.equal(createHash('sha256').update(await readFile(original)).digest('hex'), '36716b53da169936c7e1360a4fde1e2fc0c3356a6505f177235f09c5538c6e4f');
  const peer = launch(t, [original]); await peer.init(['autocomplete']);
  assert.deepEqual((await query(peer, snapshot('  /commands ex')).response).result, { prefix: 'ex', items: [{ value: 'extension', label: 'extension' }] });
  await peer.close();
});

const repo = process.env.PI_REFERENCE_REPO;
test('source-extracted Pi 1.0.2 parser and exact quote/cursor examples match, independent of the pinned TUI', {
  skip: !repo && 'set PI_REFERENCE_REPO to the local reviewed Pi reference checkout',
}, async () => {
  function source(path, hash) {
    const value = execFileSync('git', ['-C', repo, 'show', `cd32f7725fdbddbaecdff5b1e68491563394e0ca:packages/tui/src/${path}.ts`], { encoding: 'utf8', timeout: 10000 });
    assert.equal(createHash('sha256').update(value).digest('hex'), hash); return value;
  }
  const autocomplete = source('autocomplete', '7391902f35b60c3467ceb5eccc86e0954a388ea1012455c1a7ea8f7325de773b');
  const utils = source('utils', '258ff73a0ff4d2b05f8a60515a9cc37eb96c9fc03a4072ac6db2906cad1862d9');
  const pure = utils.slice(utils.indexOf('export const cjkBreakRegex'), utils.indexOf('function isPrintableAscii'))
    + autocomplete.slice(autocomplete.indexOf('const PATH_DELIMITERS'), autocomplete.indexOf('// Use fd'))
    + autocomplete.slice(autocomplete.indexOf('export class CombinedAutocompleteProvider'));
  // No imports, terminal, filesystem or process functions are supplied. Only
  // the argument branch / pure applyCompletion run; fdPath stays null.
  const js = stripTypeScriptTypes(pure); assert.doesNotMatch(js, /\bimport\s/);
  const { CombinedAutocompleteProvider } = await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);
  const seen = [];
  const provider = new CombinedAutocompleteProvider([{ name: 'complete', getArgumentCompletions(prefix) { seen.push(prefix); return [{ value: prefix, label: prefix }]; } }], '.');
  for (const prefix of ['', '  raw  "two words"', '前😀é', 'a\tb', '@file', '(@file', '（@file', '，@file', '【@file', '@"space name', 'foo@bar', '"@inside quote', '@first second', '@"two words" next']) {
    for (const lead of ['', '  ', '\t', '　']) {
      const line = `${lead}/complete ${prefix}`;
      seen.length = 0;
      const result = await provider.getSuggestions([line], 0, line.length, { signal: new AbortController().signal });
      const ours = commandArgumentRequest(snapshot(line));
      assert.equal(ours?.prefix ?? null, result?.prefix ?? null, line);
      assert.deepEqual(seen, ours ? [ours.prefix] : [], line);
    }
  }
  for (const prefix of ['@first second', '@"two words" next']) {
    const before = `/complete ${prefix}`;
    assert.deepEqual(provider.applyCompletion([before + 'TAIL'], 0, before.length, { value: prefix + '✓', label: '選択' }, prefix),
      { lines: [before + '✓ TAIL'], cursorLine: 0, cursorCol: before.length + 2 });
  }
  const quote = provider.applyCompletion(['/complete "fo" tail'], 0, '/complete "fo'.length, { value: '"folder"', label: 'folder' }, '"fo');
  assert.deepEqual(quote, { lines: ['/complete "folder" tail'], cursorLine: 0, cursorCol: '/complete "folder"'.length });
  const directory = provider.applyCompletion(['/complete "dir'], 0, '/complete "dir'.length, { value: '"目录/"', label: '目录/' }, '"dir');
  assert.deepEqual(directory, { lines: ['/complete "目录/"'], cursorLine: 0, cursorCol: '/complete "目录/'.length });
  assert.equal(Buffer.byteLength('"目录/'), 8, 'proposed cursor_offset_bytes differs from UTF-16 columns');
});

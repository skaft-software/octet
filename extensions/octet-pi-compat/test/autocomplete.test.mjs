import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { stripTypeScriptTypes } from 'node:module';
import { addAutocompleteProvider, commandCompletions, getAutocompleteProvider, retireAutocomplete } from '../lib/completions.mjs';
import { applyPiCompletion, autocompleteSnapshot, completionResponse, piPosition } from '../lib/autocomplete-edits.mjs';

const snap = (text, before = text) => autocompleteSnapshot({ text, cursor: Buffer.byteLength(before), revision: 3 });
function apply(snapshot, response, index = 0) {
  const item = response.items[index], bytes = Buffer.from(snapshot.text), start = snapshot.cursor - Buffer.byteLength(response.prefix);
  const text = Buffer.concat([bytes.subarray(0, start), Buffer.from(item.value), bytes.subarray(snapshot.cursor + (item.replace_after_bytes ?? 0))]).toString('utf8');
  return { text, cursor: start + (item.cursor_offset_bytes ?? Buffer.byteLength(item.value)) };
}
function harness() {
  let next = 1;
  const calls = [], state = { alive: true, owner: { session_id: 's', extension_instance_id: 'i', process_generation: 1 } };
  const runtime = { features: new Set(['autocomplete', 'autocomplete_edit_v1']), foreground: state, commands: new Map(),
    scope: new AsyncLocalStorage(), autocompleteRegistration: null,
    ui: { activeEditor() { return undefined; } },
    require(feature) { if (!this.features.has(feature)) throw Error(`unsupported_feature ${feature}`); },
    assertOwner(store) { if (!store.state?.alive || this.foreground !== store.state) throw Error('not_foreground_owner'); },
    transport: { request(method, params) { calls.push({ method, params }); return Promise.resolve({ accepted: true }); } },
    track(promise) { return promise; },
  };
  const store = () => ({ id: next++, state, factory: 0, controller: new AbortController(), live: true, pending: new Set(), errors: [] });
  const add = factory => { const s = store(); return runtime.scope.run(s, () => addAutocompleteProvider(runtime, s, factory)); };
  const query = (text, before = text, s = store()) => commandCompletions(runtime, { text, cursor: Buffer.byteLength(before), revision: 1 }, s);
  return { runtime, state, store, add, query, calls };
}

test('profile represents closing-quote removal, intra-value Unicode cursor, tabs and multiline edits exactly', () => {
  const quote = snap('/complete "fo" tail', '/complete "fo');
  const result = completionResponse(quote, { prefix: '"fo', items: [{ value: '"folder"', label: 'folder' }] }, { applyCompletion: applyPiCompletion }, true);
  assert.deepEqual(result, { prefix: '"fo', items: [{ value: '"folder"', label: 'folder', replace_after_bytes: 1 }] });
  assert.deepEqual(apply(quote, result), { text: '/complete "folder" tail', cursor: Buffer.byteLength('/complete "folder"') });
  const directory = snap('😀\n/complete "dir');
  const dir = completionResponse(directory, { prefix: '"dir', items: [{ value: '"目录/"', label: '目录/' }] }, { applyCompletion: applyPiCompletion }, true);
  assert.equal(dir.items[0].cursor_offset_bytes, Buffer.byteLength('"目录/'));
  assert.deepEqual(apply(directory, dir), { text: '😀\n/complete "目录/"', cursor: Buffer.byteLength('😀\n/complete "目录/') });
  const original = snap('前😀\n#oldTAIL\nlast', '前😀\n#old');
  const multiline = completionResponse(original, { prefix: '#old', items: [{ value: 'ignored', label: 'choice' }] }, {
    applyCompletion() { return { lines: ['前😀', '甲\t乙', '😀TAIL', 'last'], cursorLine: 2, cursorCol: 2 }; },
  }, true);
  assert.deepEqual(apply(original, multiline), { text: '前😀\n甲\t乙\n😀TAIL\nlast', cursor: Buffer.byteLength('前😀\n甲\t乙\n😀') });
  assert.equal(multiline.items[0].value, '甲\t乙\n😀');
});

test('request and application cursor conversions never floor bytes, UTF-16 surrogates, or combining marks', () => {
  const original = snap('前😀\né後', '前😀\né');
  assert.deepEqual([original.cursorLine, original.cursorCol], [1, 2]);
  assert.deepEqual(piPosition(original.lines, original.cursorLine, original.cursorCol), { text: original.text, index: 6 });
  assert.throws(() => autocompleteSnapshot({ text: '😀', cursor: 2, revision: 0 }), /UTF-8 byte boundary/);
  for (const col of [-1, 0.5, 1, 3]) assert.throws(() => piPosition(['😀'], 0, col), /UTF-16 scalar boundary/);
  // A scalar boundary inside a grapheme is kept exact. Native acceptance may
  // refuse it; the adapter MUST NOT silently move it to a grapheme edge.
  const response = completionResponse(snap('x'), { prefix: 'x', items: [{ value: 'é', label: 'combining' }] }, {
    applyCompletion: () => ({ lines: ['é'], cursorLine: 0, cursorCol: 1 }),
  }, true);
  assert.equal(response.items[0].cursor_offset_bytes, 1);
  assert.equal(apply(snap('x'), response).cursor, 1);
  assert.throws(() => completionResponse(snap('x'), { prefix: 'x', items: [{ value: '😀', label: 'bad' }] }, {
    applyCompletion: () => ({ lines: ['😀'], cursorLine: 0, cursorCol: 1 }),
  }, true), /UTF-16 scalar boundary/);
});

test('different per-item edits share an exact common prefix and retain each original suffix/cursor', () => {
  const original = snap('first\n#xTAIL', 'first\n#x');
  const response = completionResponse(original, { prefix: '#x', items: [{ value: 'one', label: 'one' }, { value: 'two', label: 'two' }] }, {
    applyCompletion(_lines, _line, _col, item) {
      return item.value === 'one' ? { lines: ['first', '#oneTAIL'], cursorLine: 1, cursorCol: 4 }
        : { lines: ['changed', '#two'], cursorLine: 0, cursorCol: 3 };
    },
  }, true);
  assert.equal(response.prefix, 'first\n#x');
  assert.deepEqual(apply(original, response, 0), { text: 'first\n#oneTAIL', cursor: Buffer.byteLength('first\n#one') });
  assert.deepEqual(apply(original, response, 1), { text: 'changed\n#two', cursor: 3 });
});

test('bounded edit grammar refuses unsupported fields, wrong prefixes, oversized resulting drafts and async application', () => {
  assert.throws(() => piPosition(Array(1024).fill('x'.repeat(1024)), 0, 0), /bounds_exceeded autocomplete editor text/);
  const provider = { applyCompletion: applyPiCompletion };
  const original = snap('x');
  assert.throws(() => completionResponse(original, { prefix: 'wrong', items: [{ value: 'y', label: 'y' }] }, provider, true), /exact suffix/);
  for (const value of ['\x1b', '\u0085', '\ud800', 'x'.repeat(1025)]) {
    assert.throws(() => completionResponse(original, { prefix: 'x', items: [{ value, label: 'y' }] }, provider, true));
  }
  assert.throws(() => completionResponse(original, { prefix: 'x', items: [{ value: 'y', label: '\n' }] }, provider, true), /single-line/);
  assert.throws(() => completionResponse(original, { prefix: 'x', items: [{ value: 'y', label: 'y', effect: true }] }, provider, true), /option would not be honored/);
  assert.throws(() => completionResponse(original, { prefix: 'x', items: [{ value: 'y', label: 'y' }] }, { applyCompletion: () => Promise.resolve({}) }, true), /synchronous/);
  assert.throws(() => completionResponse(original, { prefix: 'x', items: [{ value: 'y', label: 'y' }] }, {
    applyCompletion: () => ({ lines: ['x'.repeat(262145)], cursorLine: 0, cursorCol: 0 }),
  }, true), /bounds_exceeded/);
  assert.throws(() => completionResponse(snap('a\tb'), { prefix: 'a\tb', items: [{ value: 'x', label: 'x' }] }, provider, false), /single-line/);
});

test('ordered provider factories rebuild like Pi, run getSuggestions/applyCompletion with owner scope, preserve fallback', async () => {
  const h = harness(), builds = [], calls = [];
  h.runtime.commands.set('complete', { factory: 4, definition: { getArgumentCompletions: prefix => [{ value: prefix + '✓', label: 'command' }] } });
  h.add(current => {
    builds.push('first');
    return { triggerCharacters: ['#'],
      async getSuggestions(lines, line, col, options) {
        calls.push(['first', h.runtime.scope.getStore().factory, line, col]);
        if (lines[line].slice(0, col).endsWith('#é😀')) return { prefix: '#é😀', items: [{ value: '#42', label: 'issue' }] };
        return current.getSuggestions(lines, line, col, options);
      },
      applyCompletion: (...args) => current.applyCompletion(...args),
      shouldTriggerFileCompletion: (...args) => current.shouldTriggerFileCompletion(...args),
    };
  });
  h.add(current => {
    builds.push('second');
    return { triggerCharacters: ['#', '!'], getSuggestions: (...args) => { calls.push(['second']); return current.getSuggestions(...args); },
      applyCompletion: (...args) => { calls.push(['apply-second']); return current.applyCompletion(...args); },
      shouldTriggerFileCompletion: (...args) => current.shouldTriggerFileCompletion(...args) };
  });
  assert.deepEqual(builds, ['first', 'first', 'second']);
  assert.deepEqual(h.calls, [{ method: 'ui/autocomplete/register', params: { revision: 1 } }]);
  const s = h.store(), provider = getAutocompleteProvider(h.runtime, s);
  assert.deepEqual(provider.triggerCharacters, ['#', '!']);
  assert.equal(h.runtime.scope.run(s, () => provider.shouldTriggerFileCompletion(['/command'], 0, 8)), false);
  assert.equal(h.runtime.scope.run(s, () => provider.shouldTriggerFileCompletion(['src/'], 0, 4)), true);
  const text = 'head😀\n#é😀tail', before = 'head😀\n#é😀';
  const response = await h.query(text, before);
  assert.deepEqual(calls.slice(0, 2), [['second'], ['first', 0, 1, 4]]);
  assert.ok(calls.some(([name]) => name === 'apply-second'));
  assert.deepEqual(apply(snap(text, before), response), { text: 'head😀\n#42tail', cursor: Buffer.byteLength('head😀\n#42') });
  assert.equal((await h.query('/complete arg')).items[0].value, 'arg✓');
  for (const text of ['src/fi', '@file', '/unknown x']) assert.deepEqual(await h.query(text), { prefix: '', items: [] });
  assert.equal(await h.runtime.scope.run(s, () => provider.getSuggestions(['/complete arg'], 0, 13, { signal: s.controller.signal, force: true })), null);
});

test('provider admission is feature-gated and stale/retired owner results cannot escape', async () => {
  const h = harness();
  h.runtime.features.delete('autocomplete_edit_v1');
  assert.throws(() => h.add(current => current), /autocomplete_edit_v1/);
  h.runtime.features.add('autocomplete_edit_v1');
  let release, entered;
  const ready = new Promise(resolve => { entered = resolve; });
  h.add(current => ({ ...current, async getSuggestions(_lines, _line, _col, options) {
    entered(options.signal); await new Promise(resolve => { release = resolve; }); return { prefix: 'x', items: [{ value: 'y', label: 'y' }] };
  } }));
  const pending = h.query('x');
  const signal = await ready;
  retireAutocomplete(h.state); h.state.alive = false;
  assert.equal(signal.aborted, true);
  await assert.rejects(pending, /owner retired/);
  release();
  assert.throws(() => h.add(current => current), /not_foreground_owner/);
});

test('pending provider snapshots remain independent and replacement rejects old results', async () => {
  const h = harness(); let release, entered;
  const ready = new Promise(resolve => { entered = resolve; });
  h.add(current => ({ ...current, async getSuggestions(lines, line, col, options) {
    if (lines[0] === 'old') { entered(); await new Promise(resolve => { release = resolve; }); }
    options.signal.throwIfAborted(); return { prefix: lines[0], items: [{ value: 'result', label: 'result' }] };
  } }));
  const old = h.query('old'); await ready;
  assert.equal((await h.query('new')).prefix, 'new');
  h.add(current => current); release();
  await assert.rejects(old, /chain replaced/);
});

const repo = process.env.PI_REFERENCE_REPO;
test('exact native edits differential against pinned Pi 1.0.2 applyCompletion source', {
  skip: !repo && 'set PI_REFERENCE_REPO to the reviewed offline Pi checkout',
}, async () => {
  const source = execFileSync('git', ['-C', repo, 'show', 'cd32f7725fdbddbaecdff5b1e68491563394e0ca:packages/tui/src/autocomplete.ts'], { encoding: 'utf8' });
  assert.equal(createHash('sha256').update(source).digest('hex'), '7391902f35b60c3467ceb5eccc86e0954a388ea1012455c1a7ea8f7325de773b');
  const pure = stripTypeScriptTypes(source.slice(source.indexOf('export class CombinedAutocompleteProvider')));
  const { CombinedAutocompleteProvider } = await import(`data:text/javascript;base64,${Buffer.from(pure).toString('base64')}`);
  const oracle = new CombinedAutocompleteProvider([], '.');
  for (const [prefix, item] of [
    ['/co', { value: 'command', label: 'command' }], ['@first second', { value: '@文件', label: '文件' }],
    ['@"dir', { value: '@"目录/"', label: '目录/' }], ['"dir', { value: '"目录/"', label: '目录/' }],
    ['前😀', { value: '後😀', label: 'text' }], ['a\tb', { value: 'c\td', label: 'text' }],
    ['"fo', { value: '"folder"', label: 'folder' }],
  ]) for (const before of ['', '/complete ']) for (const after of ['', '" tail', '後😀']) {
    const text = `first😀\n${before}${prefix}${after}\nlast`, cursor = `first😀\n${before}${prefix}`;
    const snapshot = snap(text, cursor);
    const expected = oracle.applyCompletion([...snapshot.lines], snapshot.cursorLine, snapshot.cursorCol, item, prefix);
    assert.deepEqual(applyPiCompletion([...snapshot.lines], snapshot.cursorLine, snapshot.cursorCol, item, prefix), expected);
    const expectedPosition = piPosition(expected.lines, expected.cursorLine, expected.cursorCol);
    const response = completionResponse(snapshot, { prefix, items: [item] }, { applyCompletion: applyPiCompletion }, true);
    assert.deepEqual(apply(snapshot, response), { text: expectedPosition.text, cursor: Buffer.byteLength(expectedPosition.text.slice(0, expectedPosition.index)) });
  }
});

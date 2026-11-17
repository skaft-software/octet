import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { Editor } from '../lib/editor.mjs';
import { editorFrame } from '../lib/editor-frame.mjs';
import { RemoteTUI } from '../lib/remote-ui.mjs';
import { inRender, rendering } from '../lib/ui-delivery.mjs';
import { nativeEditorHost } from './native-editor-host.mjs';

const plain = lines => lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '').replaceAll('\x1b_pi:c\x07', '');

test('large draft and escaped completion values keep paint observations within the physical frame bound', t => {
  const peer = nativeEditorHost(t), surface_id = 'paint-bound';
  peer.call('ui/open', { surface_id });
  const request = (operation, editor_id) => peer.call('ui/chrome', { chrome: { kind: 'editor', surface_id, editor_id, operation } });
  const { editor_id } = request({ op: 'create', padding_x: 0, autocomplete_max_visible: 5 });
  const call = operation => request(operation, editor_id);
  const text = 'a'.repeat(200000) + '@x', value = '"'.repeat(4096);
  call({ op: 'set_text', text });
  const { query } = call({ op: 'query', force: true });
  const items = Array.from({ length: 110 }, (_, id) => ({ id, value, label: 'item' }));
  const suggestions = { op: 'suggestions', query_id: query.id, prefix: 'x', items };
  assert.ok(Buffer.byteLength(JSON.stringify(suggestions)) < 1048576);
  assert.equal(call(suggestions).accepted, true);
  const paint = call({ op: 'read', field: 'paint' });
  assert.ok(Buffer.byteLength(JSON.stringify({ jsonrpc: '2.0', id: 'paint', result: paint }) + '\n') <= 1048576);
  assert.equal(paint.value.text, text);
  assert.equal(paint.value.autocomplete.items.length, items.length);
  assert.equal(paint.value.autocomplete.items[0].label, 'item');
  assert.equal(paint.value.autocomplete.items[0].value, '', 'paint does not copy completion insertion values');
  assert.equal(call({ op: 'selection' }).selection.id, 0);
  call({ op: 'input', data: '', action: 'down', disable_submit: false });
  assert.equal(call({ op: 'selection' }).selection.id, 1, 'native menu selection keeps original callback handles');
  assert.equal(call({ op: 'read', field: 'paint' }).value.autocomplete.selected, 1);
});
for (const placement of ['editor', 'fullscreen']) test(`${placement} Editor paints native observations without synchronous host I/O`, async t => {
  const peer = nativeEditorHost(t), calls = [];
  const store = { id: 1, live: true, controller: new AbortController(), state: {
    alive: true, owner: { session_id: 's', extension_instance_id: 'i', process_generation: 1 },
  } };
  const surface = { id: 'paint', mountId: 'mount', placement, store, opened: true, closed: false,
    columns: 40, rows: 24, requestRender() {}, runtime: {
      scope: new AsyncLocalStorage(), require() {}, assertOwner() {},
      transport: { requestSync(method, params) {
        calls.push({ method, operation: params.chrome.operation.op, painting: rendering() });
        // A delayed host must never be consulted by a synchronous paint call.
        assert.equal(rendering(), false, 'native host request reached component render()');
        return peer.call(method, params);
      } },
    } };
  peer.call('ui/open', { surface_id: surface.id });
  const tui = new RemoteTUI(surface).facade;
  const identity = s => s;
  const editor = new Editor(tui, { borderColor: identity, selectList: {
    selectedPrefix: identity, selectedText: identity, description: identity, scrollInfo: identity, noMatch: identity,
  } });
  editor.focused = true;
  editor.setText('native draft');
  editor.borderColor = text => { editor.getCursor(); return text; };
  const before = calls.length;
  const first = inRender(() => editor.render(40));
  assert.match(plain(first), /native draft/);
  assert.equal(calls.length, before);
  assert.equal(inRender(() => editor.getText()), 'native draft');
  const observed = inRender(() => editor.getLines());
  observed[0] = 'not an edit';
  assert.equal(inRender(() => editor.getText()), 'native draft');
  assert.equal(calls.length, before, 'render-time getters use the same read-only observation');
  assert.match(plain(editor.render(40)), /native draft/, 'direct paints also keep border callbacks local');
  assert.throws(() => inRender(() => editor.setText('paint is not an edit')), /native Editor operation during render/);
  assert.equal(calls.length, before);
  editor.handleInput('!');
  const changed = calls.length;
  assert.match(plain(inRender(() => editor.render(40))), /native draft!/);
  assert.equal(calls.length, changed);
  editor.setPaddingX(1);
  assert.equal(plain(inRender(() => editor.render(9))).split('\n').every(line => line.length === 9), true);
  // The last painted geometry must reach native cursor navigation outside paint.
  editor.handleInput('\x1b[A');
  assert.deepEqual(editor.getCursor(), { line: 0, col: 5 });
  editor.setText('next');
  assert.match(plain(inRender(() => editor.render(40))), /next/);
  // Pi SelectList paints labels, falling back to values only for empty labels;
  // accepting a completion still passes the original item to the provider.
  const items = [{ value: '"'.repeat(4096), label: 'display label', description: 'description' },
    { value: 'fallback value', label: '' }];
  editor.setAutocompleteProvider({
    getSuggestions: () => ({ items, prefix: 'x' }),
    applyCompletion(_lines, _line, _col, item) {
      assert.equal(item, items[0]);
      return { lines: [item.value], cursorLine: 0, cursorCol: item.value.length };
    },
  });
  editor.setText('@x');
  editor.handleInput('\t');
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(editor.isShowingAutocomplete(), true);
  const beforeMenuPaint = calls.length, menu = plain(inRender(() => editor.render(40)));
  assert.match(menu, /display label/);
  assert.match(menu, /fallback value/);
  assert.doesNotMatch(menu, /"{2}/);
  assert.equal(calls.length, beforeMenuPaint);
  editor.handleInput('\t');
  assert.equal(editor.getText(), items[0].value);
  assert.equal(calls.some(call => call.painting), false);
});

test('local frame layout matches bounded native projections across geometry, Unicode, carets, paste atoms and menus', t => {
  const peer = nativeEditorHost(t), surface_id = 'layout';
  peer.call('ui/open', { surface_id });
  const request = (operation, editor_id) => peer.call('ui/chrome', { chrome: { kind: 'editor', surface_id, editor_id, operation } });
  const { editor_id } = request({ op: 'create', padding_x: 0, autocomplete_max_visible: 5 });
  const call = operation => request(operation, editor_id);
  for (const text of ['', 'a  word   then words ', 'word\uFEFFnext\u00A0word rest', 'α👩‍💻 界\nlast\n', '中日文本かな', Array.from({ length: 25 }, (_, i) => `row ${i}`).join('\n'), 'x'.repeat(1200)]) {
    call({ op: 'set_text', text: text.length >= 1000 ? '' : text });
    if (text.length >= 1000) call({ op: 'input', data: `\x1b[200~${text}\x1b[201~`, action: null, disable_submit: false });
    let previousScroll = 0;
    for (const padding of [0, 1, 3]) {
      call({ op: 'padding', value: padding });
      for (const caret of ['end', 'left', 'home']) {
        if (caret !== 'end') call({ op: 'input', data: '', action: caret, disable_submit: false });
        for (const rows of [5, 24]) for (const width of [1, 2, 3, 7, 16, 40]) {
          const native = call({ op: 'render', width, rows });
          const snapshot = call({ op: 'read', field: 'paint' }).value;
          const { scroll, ...local } = editorFrame(snapshot, width, rows, previousScroll);
          previousScroll = scroll;
          assert.deepEqual(local, native, JSON.stringify({ text: text.slice(0, 40), padding, caret, rows, width }));
          assert.equal(scroll, native.hidden_above);
        }
      }
    }
  }
  call({ op: 'set_text', text: '@a' });
  const query = call({ op: 'query', force: false }).query;
  call({ op: 'suggestions', query_id: query.id, prefix: 'a', items: [{ id: 0, value: 'abc', label: 'abc' }] });
  const native = call({ op: 'render', width: 16, rows: 24 });
  const { scroll: _scroll, ...local } = editorFrame(call({ op: 'read', field: 'paint' }).value, 16, 24, 0);
  assert.deepEqual(local, native);
});

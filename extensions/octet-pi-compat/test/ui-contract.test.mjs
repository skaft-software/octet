import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { dialogAPI, editorAPI, BorderedLoader, DynamicBorder, getSettingsListTheme } from '../lib/ui-api.mjs';
import { keyHint } from '../lib/keybinding-hints.mjs';
import { RemoteUI, fitLines, safeLines } from '../lib/remote-ui.mjs';
import { theme } from '../lib/theme.mjs';
import { getAutocompleteProvider, retireAutocomplete } from '../lib/completions.mjs';

function harness(t) {
  const state = { key: 'actual-owner', alive: true, owner: { session_id: 'session', extension_instance_id: 'instance', process_generation: 1 }, host: {} };
  const store = { id: 7, factory: 'fixture', state, method: 'hook/run', controller: new AbortController(), pending: new Set(), errors: [] };
  const frames = [], requests = [], errors = [];
  const runtime = {
    scope: new AsyncLocalStorage(), features: new Set(['remote_ui', 'composer', 'message_injection', 'autocomplete', 'autocomplete_edit_v1']), commands: new Map(),
    require(feature) { assert.ok(this.features.has(feature), feature); },
    assertOwner(s) { assert.ok(s.state === state && state.alive, 'current real owner'); },
    track(promise) { return Promise.resolve(promise); }, backgroundError(error) { errors.push(error); },
    timers: { surface() {} },
    transport: { async notify(method, params) { frames.push({ method, params }); }, async request(method) { assert.equal(method, 'ui/autocomplete/register'); return { accepted: true }; } },
    async hostCall(method, params) {
      requests.push({ method, params });
      if (method === 'ui/open') return { columns: 37, rows: 12, ...(params.placement === 'editor' ? { editor_mount_id: 'actual-mount' } : {}) };
      if (method === 'composer/get') return { text: 'host draft' };
      if (params.editor_checkpoint) return params.editor_checkpoint;
      return {};
    },
  };
  const ui = runtime.ui = new RemoteUI(runtime);
  t.after(async () => { await ui.shutdown(); assert.deepEqual(errors, []); });
  const custom = factory => {
    let done, reject;
    const value = new Promise((resolve, fail) => { done = resolve; reject = fail; });
    const mounting = ui.mount(store, 'fullscreen', 'Dialog', factory, { done, reject });
    return Promise.all([mounting, value]).then(([, result]) => result);
  };
  const dialog = dialogAPI(custom);
  const untilFrame = async () => {
    for (let attempt = 0; attempt < 100 && !frames.length; attempt++) await new Promise(resolve => setImmediate(resolve));
    assert.ok(frames.length, 'remote component published a real-width safe frame');
    return frames.at(-1).params;
  };
  return { state, store, runtime, ui, dialog, frames, requests, untilFrame };
}

test('dialog pre-abort resolves Pi cancel values without opening a surface', async t => {
  const h = harness(t), controller = new AbortController(); controller.abort();
  assert.equal(await h.dialog.select('Select', ['One'], { signal: controller.signal }), undefined);
  assert.equal(await h.dialog.input('Input', 'accepted-but-unused', { signal: controller.signal }), undefined);
  assert.equal(await h.dialog.confirm('Confirm', 'Details', { signal: controller.signal }), false);
  assert.equal(h.requests.length, 0);
});

test('select clamps navigation, preserves SGR and renders at host width', async t => {
  const h = harness(t);
  const result = h.dialog.select('Long title '.repeat(6), ['One', 'Two']);
  const frame = await h.untilFrame();
  assert.equal(frame.columns, 37); assert.deepEqual(safeLines(frame.lines), frame.lines);
  assert.ok(frame.lines.length > 3, 'title wrapped at real width');
  const surface = h.ui.surfaces.get(frame.surface_id);
  surface.tui.input('\x1b[A'); surface.tui.input('\r');
  assert.equal(await result, 'One');
  assert.equal(h.ui.surfaces.size, 0);
  assert.equal(h.requests.at(-1).method, 'ui/close');
});

test('oversized truecolor component frames are clipped without retiring the Pi UI mount', async t => {
  const h = harness(t);
  const cell = '\x1b[38;2;255;255;255m\x1b[48;2;255;255;255m▀';
  const line = `${cell.repeat(500)}\x1b[0m`;
  const surface = await h.ui.mount(h.store, 'fullscreen', 'Wide RGB', () => ({ render: () => [line] }));
  const frame = h.frames.find(value => value.method === 'ui/frame').params;
  assert.equal(h.ui.surfaces.get(frame.surface_id), surface);
  assert.ok(Buffer.byteLength(frame.lines[0]) <= 16384);
  assert.ok(frame.lines[0].endsWith('\x1b[0m…'));
  assert.deepEqual(safeLines(frame.lines), frame.lines);
  assert.deepEqual(fitLines(['\x1b]8;;https://example.com/path\x07linked\x1b]8;;\x07']), ['linked']);
});

test('frame projection clips row and aggregate-text overflow while keeping snapshots bounded', () => {
  const tooManyRows = fitLines(Array.from({ length: 257 }, (_, index) => `row-${index}`));
  assert.equal(tooManyRows.length, 256);
  assert.ok(tooManyRows.at(-1).endsWith('…'));
  assert.deepEqual(safeLines(tooManyRows), tooManyRows);

  const tooMuchText = fitLines(Array.from({ length: 40 }, () => 'x'.repeat(16384)));
  assert.ok(tooMuchText.reduce((total, row) => total + Buffer.byteLength(row), 0) <= 524288);
  assert.ok(tooMuchText.at(-1).endsWith('…'));
  assert.deepEqual(safeLines(tooMuchText), tooMuchText);

  const unicode = fitLines(['😀'.repeat(5000)]);
  assert.ok(Buffer.byteLength(unicode[0]) <= 16384);
  assert.ok(unicode[0].endsWith('…'));
  assert.deepEqual(safeLines(unicode), unicode);

  const narrowRemainder = fitLines([...Array(31).fill('x'.repeat(16384)), 'y'.repeat(16382), 'z', 'w', 'x']);
  assert.ok(narrowRemainder.reduce((total, row) => total + Buffer.byteLength(row), 0) <= 524288);
  assert.ok(narrowRemainder.at(-1).endsWith('…'));
  assert.deepEqual(safeLines(narrowRemainder), narrowRemainder);
  assert.throws(() => fitLines(['\x1b]8;;javascript:alert(1)\x07unsafe\x1b]8;;\x07']), /invalid_request/);
});

test('safeLines stays strict at the existing row, line and aggregate wire limits', () => {
  assert.throws(() => safeLines(Array(257).fill('')), /bounds_exceeded ui\/frame rows/);
  assert.throws(() => safeLines(['x'.repeat(16385)]), /bounds_exceeded ui\/frame text/);
  assert.throws(() => safeLines(Array(33).fill('x'.repeat(16384))), /bounds_exceeded ui\/frame text/);
});

test('input accepts placeholder, edits actual Input and abort disposes the mount', async t => {
  const h = harness(t), controller = new AbortController();
  const result = h.dialog.input('Input', 'not-a-prefill', { signal: controller.signal });
  const frame = await h.untilFrame();
  assert.ok(!frame.lines.join('\n').includes('not-a-prefill'));
  const surface = h.ui.surfaces.get(frame.surface_id);
  surface.tui.input('héllo'); surface.tui.input('\r');
  assert.equal(await result, 'héllo');
  h.frames.length = 0;
  const cancelled = h.dialog.input('Cancelled', undefined, { signal: controller.signal });
  await h.untilFrame(); controller.abort();
  assert.equal(await cancelled, undefined); assert.equal(h.ui.surfaces.size, 0);
});

test('confirm timeout displays Pi whole-second countdown and returns false after restoration', async t => {
  const h = harness(t);
  const result = h.dialog.confirm('Confirm', 'Multiline\nquestion', { timeout: 1 });
  const frame = await h.untilFrame();
  assert.ok(frame.lines.join('\n').includes('(1s)'));
  assert.equal(await result, false);
  assert.equal(h.requests.at(-1).method, 'ui/close');
});

test('custom invokes onHandle after dynamic overlay construction; unfocus and hide preserve identity', async t => {
  const h = harness(t); let handled;
  const base = { render: () => ['base'], handleInput(data) { handled = `base:${data}`; } };
  const overlay = { width: 11, render: width => [`width:${width}`], handleInput(data) { handled = `overlay:${data}`; } };
  let handle;
  const surface = await h.ui.mount(h.store, 'fullscreen', 'Overlay', tui => { tui.addChild(base); tui.setFocus(base); return overlay; }, {
    overlayOptions: () => undefined, onHandle(value) { handle = value; assert.equal(value.isFocused(), true); },
  });
  assert.ok(h.frames.at(-1).params.lines.join('\n').includes('width:11'));
  handle.unfocus(); surface.tui.input('x'); assert.equal(handled, 'base:x');
  handle.focus(); surface.tui.input('y'); assert.equal(handled, 'overlay:y');
  handle.setHidden(true); assert.equal(handle.isHidden(), true);
  handle.setHidden(false); assert.equal(handle.isFocused(), true);
  handle.hide(); assert.equal(handle.isHidden(), true);
  handle.setHidden(false); assert.equal(handle.isHidden(), true, 'removed overlay cannot revive');
});

test('getEditorComponent preserves factory identity across contexts and clears to undefined', async t => {
  const h = harness(t), mounts = [];
  const setSlot = (...args) => { mounts.push(args); return Promise.resolve(); };
  const first = editorAPI(h.runtime, h.store, setSlot), second = editorAPI(h.runtime, { ...h.store, factory: 'second' }, setSlot);
  assert.equal(first.getEditorComponent(), undefined);
  const factory = () => ({ render() { return []; } });
  assert.equal(first.setEditorComponent(factory), undefined);
  assert.equal(second.getEditorComponent(), factory);
  second.setEditorComponent(undefined);
  assert.equal(first.getEditorComponent(), undefined);
  assert.deepEqual(mounts.map(args => args.slice(0, 2)), [['editor', 'editor'], ['editor', 'editor']]);
});

test('autocomplete wrapper chain is usable by custom editors and retired with owner', async t => {
  const h = harness(t), api = editorAPI(h.runtime, h.store, () => Promise.resolve());
  const order = [];
  h.runtime.scope.run(h.store, () => {
    api.addAutocompleteProvider(current => { order.push('first'); return { ...current, getSuggestions: () => ({ prefix: 'a', items: [{ value: 'answer', label: 'Answer' }] }) }; });
    api.addAutocompleteProvider(current => { order.push('second'); return current; });
  });
  assert.deepEqual(order, ['first', 'first', 'second']);
  const provider = getAutocompleteProvider(h.runtime, h.store);
  const result = await h.runtime.scope.run(h.store, () => provider.getSuggestions(['a'], 0, 1));
  assert.equal(result.items[0].value, 'answer');
  retireAutocomplete(h.state);
  assert.notEqual(getAutocompleteProvider(h.runtime, h.store), provider);
});

test('public authoring helpers use the existing component library and dispose loader timers', () => {
  assert.equal(new DynamicBorder(text => text).render(5)[0], '─────');
  assert.ok(keyHint('tui.select.cancel', 'cancel').includes('escape'));
  assert.equal(getSettingsListTheme().label('plain', false), 'plain');
  const loader = new BorderedLoader({ requestRender() {} }, theme, 'Working');
  let cancelled = false; loader.onAbort = () => { cancelled = true; };
  assert.deepEqual(safeLines(loader.render(37)), loader.render(37));
  loader.handleInput('\x1b'); assert.ok(loader.signal.aborted && cancelled); loader.dispose();
  const fixed = new BorderedLoader({ requestRender() {} }, theme, 'Working', { cancellable: false });
  fixed.handleInput('\x1b'); assert.equal(fixed.signal.aborted, false); fixed.dispose();
});

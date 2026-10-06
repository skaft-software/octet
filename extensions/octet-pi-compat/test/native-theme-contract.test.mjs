import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { createNativeTheme, contextTheme, bindHostTheme, theme } from '../lib/theme.mjs';
import { foregroundTokens, backgroundTokens } from '../lib/theme-palette.mjs';
import { chromeAPI } from '../lib/ui-api.mjs';
import { owner } from './helper.mjs';

export function palette(name = 'native-dark', color = '#102030', overrides = {}) {
  const foregrounds = Object.fromEntries(foregroundTokens.map(token => [token, { color, dim: token === 'muted' }]));
  const backgrounds = Object.fromEntries(backgroundTokens.map(token => [token, '#405060']));
  return { name, path: null, appearance: 'dark', colors: { ...Object.fromEntries(foregroundTokens.map(token => [token, color])), ...backgrounds },
    foregrounds, backgrounds, capabilities: { color: 'truecolor', bold: true, dim: true, italic: false, underline: true, inverse: false, strikethrough: false }, ...overrides };
}
function harness() {
  const store = { id: 7, live: true, controller: new AbortController(), state: { owner, alive: true, host: { theme: palette() } } };
  const calls = [];
  const runtime = { scope: new AsyncLocalStorage(), ui: { surfaces: new Map() }, features: new Set(['remote_ui']), require: feature => assert.equal(feature, 'remote_ui'),
    assertOwner: s => assert.equal(s.state, store.state), assertSessionOwner: s => assert.ok(s.state.alive),
    transport: { requestSync(method, params, options) { calls.push({ method, params, options }); return runtime.receipt; } } };
  const ui = chromeAPI(runtime, store), current = contextTheme(runtime, store);
  return { runtime, store, calls, ui, current };
}

test('resolved native colors, appearance and faint resets honor disabled modifiers', () => {
  const loaded = createNativeTheme(palette('native-light', '#102030', { appearance: 'light' }));
  assert.equal(loaded.appearance, 'light');
  assert.equal(loaded.fg('muted', 'x'), '\x1b[38;2;16;32;48m\x1b[2mx\x1b[22;39m');
  assert.equal(loaded.italic('x'), 'x');
  assert.equal(loaded.inverse('x'), 'x');
  assert.equal(loaded.strikethrough('x'), 'x');
  assert.doesNotMatch(loaded.style('x', { fg: 'muted', italic: true, inverse: true, strikethrough: true }), /\x1b\[(3|7|9|23|27|29)m/);
  const plain = createNativeTheme(palette('none', '#102030', { capabilities: { color: 'none', bold: false, dim: false, italic: false, underline: false, inverse: false, strikethrough: false } }));
  assert.equal(plain.fg('muted', 'x'), 'x'); assert.equal(plain.bg('selectedBg', 'x'), 'x');
  assert.equal(plain.style('x', { fg: 'text', bg: 'selectedBg', bold: true, dim: true, underline: true }), 'x');
});

test('named lookup is independent, missing undefined, metadata catalog contains no palettes', () => {
  const h = harness(); h.runtime.receipt = { theme: palette('other', '#abcdef') };
  const named = h.ui.getTheme('other');
  assert.equal(named.name, 'other'); assert.equal(h.current.name, 'native-dark');
  assert.deepEqual(h.calls[0].params, { parent_request_id: 7, resource_owner: owner, chrome: { kind: 'theme_get', name: 'other' } });
  h.runtime.receipt = { theme: null }; assert.equal(h.ui.getTheme('missing'), undefined);
  h.runtime.receipt = { themes: [{ name: 'other', path: '/reviewed/other.toml' }, { name: 'native-dark' }] };
  assert.deepEqual(h.ui.getAllThemes(), [{ name: 'other', path: '/reviewed/other.toml' }, { name: 'native-dark', path: undefined }]);
});

test('selection is visible before synchronous return; failure and malformed success preserve palette', () => {
  const h = harness(); h.runtime.receipt = { success: true, theme: palette('actual', '#abcdef') };
  assert.deepEqual(h.ui.setTheme('requested'), { success: true });
  assert.equal(h.current.name, 'actual'); assert.match(h.current.fg('text', 'x'), /171;205;239/);
  h.runtime.receipt = { success: false, error: 'missing theme', theme: palette('unrelated') };
  assert.deepEqual(h.ui.setTheme('missing'), { success: false, error: 'missing theme' }); assert.equal(h.current.name, 'actual');
  h.runtime.receipt = { success: true, theme: { name: 'invalid' } };
  assert.throws(() => h.ui.setTheme('invalid')); assert.equal(h.current.name, 'actual');
  const count = h.calls.length;
  assert.equal(h.ui.setTheme(createNativeTheme(palette('object'))).success, false); assert.equal(h.calls.length, count);
});

test('concurrent imported theme helpers follow ALS owner, retained context follows its own owner', async () => {
  const h = harness(); bindHostTheme(h.runtime);
  const foreign = { ...h.store, state: { alive: true, host: { theme: palette('foreign') } } };
  await Promise.all([h.runtime.scope.run(h.store, async () => { await Promise.resolve(); assert.equal(theme.name, 'native-dark'); }),
    h.runtime.scope.run(foreign, async () => { await Promise.resolve(); assert.equal(theme.name, 'foreign'); })]);
  assert.equal(h.current.name, 'native-dark');
  h.store.state.host.theme = palette('updated'); assert.equal(h.current.name, 'updated');
});

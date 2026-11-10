import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';
import { safeLines } from '../lib/remote-ui.mjs';
import { visibleWidth } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';
import { Runtime } from '../lib/runtime.mjs';
import { createContext } from '../lib/api.mjs';

async function fixture(t, options = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'octet-status-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const entry = join(directory, 'status.mjs');
  await writeFile(entry, `let captured;
export default pi => {
  pi.on('session_start', (_, ctx) => { captured ??= ctx; if (!ctx.sessionId.includes('replacement') && ${!options.skipStartupStatus}) ctx.ui.setStatus('ponytail', '○ 🐴 ponytail: ⚡ FULL'); });
  pi.on('session_shutdown', (_, ctx) => ctx.ui.setStatus('ending', 'SAVING'));
  pi.registerCommand('status', {handler: (args, ctx) => {
    const [key, ...text] = args.split(' '); ctx.ui.setStatus(key, text.length ? text.join(' ') : undefined);
  }});
  pi.registerCommand('footer', {handler: (_, ctx) => ctx.ui.setFooter((t, theme, data) => ({render: () => ['CUSTOM ' + [...data.getExtensionStatuses().values()].join(' ')], invalidate() {}}))});
  pi.registerCommand('clear-footer', {handler: (_, ctx) => ctx.ui.setFooter(undefined)});
  pi.registerCommand('stale', {handler: () => captured.ui.setStatus('stale', 'STALE')});
  pi.registerCommand('bounds', {handler: (_, ctx) => {
    for (let i = 0; i < 64; i++) ctx.ui.setStatus('key' + i, 'x');
  }});
  pi.registerCommand('unsafe', {handler: (_, ctx) => ctx.ui.setStatus('unsafe', '\\x1b]2;unsafe\\x07')});
};`);
  const peer = launch(t, [entry], options);
  await peer.init(options.headless ? [] : undefined);
  await peer.start(options.startHost);
  return peer;
}
const plain = frame => frame.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '');
const statusFrame = text => frame => frame.method === 'ui/frame' && plain(frame).includes(text);
async function command(peer, name, args = []) {
  const result = await peer.command(name, args).response;
  assert.ok(result.result, JSON.stringify(result));
}

test('ambient keyed statuses render one ordered row below the default footer and clear independently', async t => {
  const peer = await fixture(t);
  const loaded = await peer.wait(statusFrame('ponytail: ⚡ FULL'));
  const open = await peer.wait(f => f.method === 'ui/open');
  assert.equal(open.params.placement, 'below_editor');
  assert.deepEqual(loaded.params.resource_owner, owner);
  await command(peer, 'status', ['z', 'LAST']);
  await command(peer, 'status', ['a', 'FIRST']);
  const ordered = await peer.wait(statusFrame('FIRST ○ 🐴 ponytail: ⚡ FULL LAST'));
  assert.equal(ordered.params.lines.length, 1);
  assert.equal(peer.seen.filter(f => f.method === 'ui/open').length, 1);
  await command(peer, 'status', ['ponytail', '○ 🐴 ponytail: 🔥 ULTRA']);
  await peer.wait(statusFrame('ponytail: 🔥 ULTRA'));
  await command(peer, 'status', ['a']);
  await command(peer, 'status', ['z']);
  await command(peer, 'status', ['ponytail']);
  const close = await peer.wait(f => f.method === 'ui/close');
  assert.equal(close.params.surface_id, open.params.surface_id);
  await peer.close();
});

test('custom footer owns statuses without a duplicate ambient row; clearing it restores statuses', async t => {
  const peer = await fixture(t);
  const original = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'below_editor');
  await command(peer, 'footer');
  await peer.wait(statusFrame('CUSTOM ○ 🐴 ponytail: ⚡ FULL'));
  await peer.wait(f => f.method === 'ui/close' && f.params.surface_id === original.params.surface_id);
  const opens = peer.seen.filter(f => f.method === 'ui/open').length;
  await command(peer, 'status', ['ponytail', 'ULTRA']);
  await peer.wait(statusFrame('CUSTOM ULTRA'));
  assert.equal(peer.seen.filter(f => f.method === 'ui/open').length, opens);
  await command(peer, 'clear-footer');
  const restored = await peer.wait(f => f.method === 'ui/open' && f.params.placement === 'below_editor');
  assert.notEqual(restored.params.surface_id, original.params.surface_id);
  await peer.wait(f => statusFrame('ULTRA')(f) && f.params.surface_id === restored.params.surface_id);
  await peer.close();
});

test('status snapshots preserve safe SGR, normalize newlines and stay one row at narrow and large widths', async t => {
  const peer = await fixture(t);
  const loaded = await peer.wait(statusFrame('ponytail: ⚡ FULL'));
  await command(peer, 'status', ['ponytail', '\x1b[31mRED\x1b[0m\nSECOND']);
  const styled = await peer.wait(statusFrame('RED SECOND'));
  assert.ok(styled.params.lines[0].includes('\x1b[31m'));
  const text = '\x1b[31m界🔥' + 'long'.repeat(500) + '\x1b[0m\nSECOND';
  await command(peer, 'status', ['ponytail', text]);
  for (const columns of [1, 19, 120]) {
    peer.notify('ui/resize', {surface_id: loaded.params.surface_id, columns, rows: 8});
    const frame = await peer.wait(f => f.method === 'ui/frame' && f.params.columns === columns && f.params.revision > loaded.params.revision);
    assert.equal(frame.params.lines.length, 1);
    assert.ok(visibleWidth(frame.params.lines[0]) <= columns);
    assert.deepEqual(safeLines(frame.params.lines), frame.params.lines);
  }
  await peer.close();
});

test('unsafe status controls and aggregate entry overflow refuse without replacing accepted entries', async t => {
  const peer = await fixture(t);
  assert.equal((await peer.command('unsafe').response).error?.code, -32602);
  for (const text of ['\x1b[2J', 'bad\0status', 'bad\x07status', 'bad\x08status']) {
    assert.equal((await peer.command('status', ['ponytail', text]).response).error?.code, -32602);
  }
  await command(peer, 'footer');
  await peer.wait(statusFrame('CUSTOM ○ 🐴 ponytail: ⚡ FULL'));
  assert.equal((await peer.command('status', ['ponytail', '界'.repeat(1366)]).response).error?.code, -32602);
  assert.equal((await peer.command('status', ['k'.repeat(65), 'too-long-key']).response).error?.code, -32602);
  assert.equal((await peer.command('bounds').response).error?.code, -32602);
  await command(peer, 'status', ['key0', 'REPLACED']);
  await peer.wait(statusFrame('REPLACED'));
  await peer.close();
});

test('owner replacement retires the old status surface and refuses captured setters', async t => {
  const peer = await fixture(t);
  const old = await peer.wait(statusFrame('ponytail: ⚡ FULL'));
  const replacement = {...owner, session_id: 'replacement'};
  const context = {...peer.context({session_id: 'replacement'}), resource_owner: replacement};
  const result = await peer.request('hook/run', {hook: 'session_start', payload: {binding: replacement}, context}).response;
  assert.ok(result.result, JSON.stringify(result));
  const stale = await peer.request('command/execute', {name: 'stale', arguments: [], context}).response;
  assert.equal(stale.error?.code, -32002);
  const changed = await peer.request('command/execute', {name: 'status', arguments: ['new', 'NEW-OWNER'], context}).response;
  assert.ok(changed.result, JSON.stringify(changed));
  const frame = await peer.wait(statusFrame('NEW-OWNER'));
  assert.deepEqual(frame.params.resource_owner, replacement);
  assert.notEqual(frame.params.surface_id, old.params.surface_id);
  assert.ok(!plain(frame).includes('ponytail'));
  await peer.close();
});

test('tabs in status text normalize to spaces while safe SGR survives', async t => {
  const peer = await fixture(t);
  await command(peer, 'status', ['ponytail', '\x1b[31malpha\t\tbeta\x1b[0m']);
  const frame = await peer.wait(statusFrame('alpha beta'));
  assert.ok(frame.params.lines[0].includes('\x1b[31m'));
  assert.equal(frame.params.lines[0].includes('\t'), false);
  assert.deepEqual(safeLines(frame.params.lines), frame.params.lines);
  await peer.close();
});

test('negotiated remote UI with has_ui false preserves startup and command metadata without UI effects', async t => {
  const peer = await fixture(t, { startHost: { has_ui: false } });
  assert.equal(peer.seen.some(f => f.method?.startsWith('ui/')), false);
  const result = await peer.command('status', ['extra', 'HEADLESS ULTRA'], { has_ui: false }).response;
  assert.ok(result.result, JSON.stringify(result));
  assert.equal(peer.seen.some(f => f.method?.startsWith('ui/')), false);
  const footer = await peer.command('footer', [], { has_ui: true }).response;
  assert.ok(footer.result, JSON.stringify(footer));
  await peer.wait(statusFrame('CUSTOM ○ 🐴 ponytail: ⚡ FULL HEADLESS ULTRA'));
  await peer.close();
});

test('shutdown status with remote UI negotiated and has_ui false does not require a foreground UI pump', async t => {
  const peer = await fixture(t, { skipStartupStatus: true });
  const result = await peer.request('hook/run', {
    hook: 'session_end', payload: { binding: owner }, context: peer.context({ has_ui: false }),
  }).response;
  assert.ok(result.result, JSON.stringify(result));
  assert.equal(peer.seen.some(f => f.method?.startsWith('ui/')), false);
  await peer.close();
});

function statusContext() {
  const runtime = new Runtime({ extensions: [] }, { notify: async () => {} });
  runtime.uninstallChildren();
  runtime.features.add('remote_ui');
  const store = { id: 1, factory: 'status-test', method: 'command/execute', controller: new AbortController(), pending: new Set(), errors: [], live: true };
  runtime.bind({ context: { workspace: '/workspace', resource_owner: owner, host: { has_ui: false } } }, store, { foreground: true });
  // Mirror a live request; presentation joins only a still-active parent.
  runtime.active.set(store.id, store);
  const effects = [];
  runtime.ui.mount = async () => { effects.push('mount'); };
  runtime.ui.clearSlot = async () => { effects.push('clear'); };
  runtime.ui.surfaces.set('existing', { store, requestRender() { effects.push('render'); } });
  return { runtime, store, effects, ui: createContext(runtime, store).ui };
}

test('UI denial skips existing-surface repaints and clears, not metadata validation or ownership fences', async () => {
  const { runtime, store, effects, ui } = statusContext();
  ui.setStatus('kept', 'KEPT');
  ui.setStatus('removed', 'REMOVED');
  ui.setStatus('removed', undefined);
  assert.throws(() => ui.setStatus('unsafe', '\x1b]2;unsafe\x07'), /unsafe escape/);
  assert.deepEqual([...store.state.statuses.values()], ['KEPT']);
  await runtime.flush(store);
  assert.deepEqual(effects, []);
  store.state.alive = false;
  assert.throws(() => ui.setStatus('retired', 'RETIRED'), /not_foreground_owner/);
  store.state.alive = true;
  store.controller.abort(new Error('cancelled status request'));
  assert.throws(() => ui.setStatus('cancelled', 'CANCELLED'), /cancelled status request/);
  assert.deepEqual([...store.state.statuses.values()], ['KEPT']);
});

test('queued ambient status synchronization rechecks UI availability before mounting', async () => {
  const { runtime, store, effects, ui } = statusContext();
  store.state.host.has_ui = true;
  ui.setStatus('kept', 'QUEUED');
  effects.length = 0;
  store.state.host.has_ui = false;
  await runtime.flush(store);
  assert.deepEqual(effects, []);
  assert.deepEqual([...store.state.statuses.values()], ['QUEUED']);
  store.state.host.has_ui = true;
  ui.setStatus('kept', 'RESTORED');
  await runtime.flush(store);
  assert.deepEqual(effects, ['render', 'mount']);
});

test('headless status metadata does not acquire a remote UI surface', async t => {
  const peer = await fixture(t, {headless: true});
  await command(peer, 'status', ['ponytail', 'ULTRA']);
  assert.equal(peer.seen.some(f => f.method?.startsWith('ui/')), false);
  await peer.close();
});

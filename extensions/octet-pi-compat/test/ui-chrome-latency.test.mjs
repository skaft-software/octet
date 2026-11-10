import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

// Pi's chrome setters are local state updates: `ExtensionUIContext` declares them
// void (packages/coding-agent/src/core/extensions/types.ts:181-226) and the
// interactive implementation only mutates local state plus requestRender()
// (modes/interactive/interactive-mode.ts:2401-2404, 2739-2752). A host that never
// answers a chrome request must therefore not hold `session_start`, not fail it,
// and not turn N setters into N round trips.
async function fixture(t, source) {
  const dir = await mkdtemp(join(tmpdir(), 'octet-pi-chrome-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'chrome.ts');
  await writeFile(entry, source);
  return entry;
}

const sessionStartSetters = `export default pi => {
  pi.on('session_start', (_, ctx) => {
    ctx.ui.setTitle('title-one');
    ctx.ui.setTitle('title-two');
    ctx.ui.setWorkingMessage('working');
    ctx.ui.setWorkingVisible(true);
    ctx.ui.setHiddenThinkingLabel('hidden');
    ctx.ui.setToolsExpanded(true);
    ctx.ui.setWorkingIndicator({ frames: ['-'], intervalMs: 80 });
    ctx.ui.setWidget('probe', ['widget line']);
    ctx.ui.setFooter((tui, theme) => ({ render: () => ['footer line'], invalidate() {} }));
    for (let i = 0; i < 25; i++) ctx.ui.setStatus('k' + i, 'v' + i);
    // Pi's setters return void and cannot fail a hook.
    console.log('chrome-report:' + JSON.stringify({ expanded: ctx.ui.getToolsExpanded() }));
  });
};`;

const count = (peer, match) => peer.seen.filter(match).length;

test('a responsive host admits a presentation slot before its parent hook settles', async t => {
  const entry = await fixture(t, `export default pi => {
    pi.on('session_start', (_, ctx) => {
      ctx.ui.setFooter(() => ({ render: () => ['ADMITTED FOOTER'], invalidate() {} }));
    });
  };`);
  const peer = launch(t, [entry], { hold: ['ui/open'] });
  await peer.init(['remote_ui', 'composer']);
  const pending = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
  let settled = false;
  pending.response.then(() => { settled = true; });
  const open = await peer.wait(f => f.method === 'ui/open');
  await new Promise(resolve => setTimeout(resolve, 25));
  assert.equal(settled, false, 'the parent must remain live while the mount is admitted');
  peer.send({ jsonrpc: '2.0', id: open.id, result: { columns: 80, rows: 24 } });
  const response = await pending.response;
  assert.ok(response.result, JSON.stringify(response));
  const frame = await peer.wait(f => f.method === 'ui/frame' && f.params.lines.join('').includes('ADMITTED FOOTER'));
  assert.equal(frame.params.surface_id, open.params.surface_id);
  await peer.close();
});

test('a dropped disclosure read has a bounded fallback and does not poison the transport', async t => {
  const entry = await fixture(t, `export default pi => {
    pi.on('session_start', (_, ctx) => console.log('disclosure:' + ctx.ui.getToolsExpanded()));
  };`);
  const peer = launch(t, [entry], { hold: ['ui/chrome'] });
  await peer.init(['remote_ui']);
  const started = Date.now();
  const response = await peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() }).response;
  assert.ok(response.result, JSON.stringify(response));
  assert.ok(Date.now() - started < 3000, 'read-only timeout must not wait for a mutation watchdog');
  await peer.waitStderr(/disclosure:false/);
  const request = await peer.wait(f => f.method === 'ui/chrome' && f.params.chrome.kind === 'get');
  await peer.wait(f => f.method === '$/cancelRequest' && f.params.id === request.id);
  // A subsequent host request still succeeds on the same transport.
  const again = await peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() }).response;
  assert.ok(again.result, JSON.stringify(again));
  await peer.close();
});

test('reading tool disclosure while painting a Pi component never asks the host', async t => {
  const entry = await fixture(t, `export default pi => {
    pi.on('session_start', (_, ctx) => {
      ctx.ui.setFooter(() => ({ render: () => ['EXPANDED:' + ctx.ui.getToolsExpanded()], invalidate() {} }));
    });
  };`);
  const peer = launch(t, [entry]);
  await peer.init(['remote_ui', 'composer']);
  const response = await peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() }).response;
  assert.ok(response.result, JSON.stringify(response));
  await peer.wait(f => f.method === 'ui/frame' && f.params.lines.join('').includes('EXPANDED:'));
  assert.equal(count(peer, f => f.method === 'ui/chrome' && f.params?.chrome?.kind === 'get'), 0,
    'a paint path must read the local value, not block on a host round trip');
  await peer.close();
});

test('a chrome request the host never answers does not hold or fail session_start', async t => {
  const entry = await fixture(t, sessionStartSetters);
  // `ui/chrome` and `ui/open` are never answered: before the fix the first
  // setTitle blocked the hook on the synchronous 30 s transport wait.
  const peer = launch(t, [entry], { hold: ['ui/chrome', 'ui/open'] });
  await peer.init(['remote_ui', 'composer', 'session_entries']);
  const started = Date.now();
  const response = await peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() }).response;
  const elapsed = Date.now() - started;
  assert.ok(response.result, JSON.stringify(response));
  assert.equal(response.result.disposition.action, 'continue');
  assert.ok(elapsed < 5000, `session_start took ${elapsed} ms with an unanswering host`);
  // Local state is applied immediately, before the host sees anything.
  await peer.waitStderr(/chrome-report:.*"expanded":true/);
  await peer.close();
});

test('a refused chrome request is one bounded diagnostic and never a hook failure', async t => {
  const entry = await fixture(t, sessionStartSetters);
  const peer = launch(t, [entry], { hold: ['ui/chrome', 'ui/open'] });
  await peer.init(['remote_ui', 'composer', 'session_entries']);
  const pending = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
  const chrome = await peer.wait(f => f.method === 'ui/chrome');
  peer.send({ jsonrpc: '2.0', id: chrome.id, error: { code: -32603, message: 'host refused chrome' } });
  const response = await pending.response;
  assert.ok(response.result, JSON.stringify(response));
  assert.equal(response.result.disposition.action, 'continue');
  // One diagnostic for the refused kind, no per-setter storm.
  await peer.waitStderr(/host refused chrome/);
  const lines = peer.stderr().split('\n').filter(line => line.includes('host refused chrome'));
  assert.ok(lines.length <= 2, `expected one bounded diagnostic, saw ${lines.length}: ${peer.stderr().slice(-800)}`);
  await peer.close();
});

test('setter bursts coalesce into one chrome delivery per kind and one statuses mount', async t => {
  const entry = await fixture(t, sessionStartSetters);
  const peer = launch(t, [entry], { hold: ['ui/chrome', 'ui/open'] });
  await peer.init(['remote_ui', 'composer', 'session_entries']);
  const response = await peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() }).response;
  assert.ok(response.result, JSON.stringify(response));
  await peer.waitStderr(/chrome-report:/);
  await new Promise(resolve => setTimeout(resolve, 200));
  const titles = count(peer, f => f.method === 'ui/chrome' && f.params.chrome.kind === 'title');
  assert.equal(titles, 1, `three setTitle calls must coalesce into one delivery, saw ${titles}`);
  const chromeFor = kind => peer.seen.filter(f => f.method === 'ui/chrome' && f.params.chrome?.kind === kind);
  assert.equal(chromeFor('title')[0].params.chrome.title, 'title-two');
  // A burst of different setters is still one request per kind, never one per call.
  for (const kind of ['working_message', 'working_visible', 'hidden_thinking', 'working_indicator', 'tools_expanded']) {
    assert.ok(chromeFor(kind).length <= 1, `${kind} must be delivered at most once, saw ${chromeFor(kind).length}`);
  }
  // 25 setStatus calls collapse into at most one status refresh. A mounted footer
  // already carries the status lines, so zero mounts is also correct here; 25 (one
  // per call) is the defect.
  const statusMounts = count(peer, f => f.method === 'ui/open' && f.params.slot === 'statuses');
  assert.ok(statusMounts <= 1, `25 setStatus calls must not each mount the statuses surface, saw ${statusMounts}`);
  // Slot mounts are presentation too: they may be admitted or cancelled when the
  // hook settles, but a burst must never multiply them.
  assert.ok(count(peer, f => f.method === 'ui/open' && f.params.slot === 'widget:probe') <= 1);
  assert.ok(count(peer, f => f.method === 'ui/open' && f.params.slot === 'footer') <= 1);
  await peer.close();
});

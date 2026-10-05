import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdtemp, writeFile, rm, readFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { inspect, launch, root, owner, host } from './helper.mjs';
import { RemoteTUI, safeLines } from '../lib/remote-ui.mjs';
import { keyData, mouseData } from '../lib/keys.mjs';
import { isKeyRelease, isKeyRepeat, matchesKey } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';
import { Transport } from '../lib/transport.mjs';
import { configure } from '../configure.mjs';

const frame = text => f => f.method === 'ui/frame' && f.params.lines.join('\n').includes(text);
async function temporary(t) { const dir = await mkdtemp(join(tmpdir(), 'octet-pi-compat-test-')); t.after(() => rm(dir, { recursive: true, force: true })); return dir; }

test('static TypeScript registrations, flags, stdout isolation, useful truthful model projection', async t => {
  const peer = launch(t); await peer.init(); await peer.start();
  const result = await peer.call('normal').response;
  assert.equal(result.result.content[0].text, `${root}|true|host-value`);
  assert.deepEqual(result.result.metadata.pi_details.model.cost, { input: 1, output: 2, cacheRead: 0.1, cacheWrite: 0.2 });
  assert.equal(result.result.metadata.pi_details.model.contextWindow, 32768);
  assert.match(peer.stderr(), /console diagnostic/); assert.match(peer.stderr(), /direct stdout diagnostic/);
  await peer.close();
});
test('hasUI is false when remote_ui was not offered and custom fails explicitly', async t => {
  const peer = launch(t); await peer.init(['request_progress']);
  assert.match((await peer.call('normal').response).result.content[0].text, /\|false\|/);
  assert.match((await peer.command('surface').response).error.message, /unsupported_feature remote_ui/);
});
test('custom settles its command after mount but keeps JS done result, timers, owner and input live', async t => {
  const peer = launch(t); await peer.init();
  const command = peer.command('surface'); const open = await peer.wait(f => f.method === 'ui/open');
  assert.equal(open.params.parent_request_id, command.id); assert.deepEqual(open.params.resource_owner, owner);
  assert.ok((await command.response).result);
  const first = await peer.wait(frame('tick='));
  const next = await peer.wait(f => frame('tick=')(f) && f.params.revision > first.params.revision);
  assert.deepEqual(next.params.resource_owner, owner);
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'w', kind: 'release', modifiers: [] });
  await peer.wait(frame('w:release'));
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'ArrowUp', kind: 'repeat', modifiers: [] });
  await peer.wait(frame('up:repeat'));
  peer.notify('ui/resize', { surface_id: open.params.surface_id, columns: 60, rows: 20 });
  await peer.wait(frame('60x20'));
  peer.notify('context/updated', { resource_owner: owner, host: { ...host, model_view: { ...host.model_view, name: 'Changed Model' }, session_name: 'Changed Session' } });
  await peer.wait(frame('Changed Model|Changed Session'));
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'q', kind: 'press', modifiers: [] });
  const close = await peer.wait(f => f.method === 'ui/close'); assert.deepEqual(close.params.resource_owner, owner);
  await peer.wait(f => f.method === 'notification' && f.params.message === 'done:completed');
  await peer.close();
});
test('genuine remote selection returns chosen value and overlays preserve component focus', async t => {
  const peer = launch(t); await peer.init(); const command = peer.command('select');
  const open = await peer.wait(f => f.method === 'ui/open'); await command.response;
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'ArrowDown', kind: 'press', modifiers: [] });
  await peer.wait(frame('→ two'));
  peer.notify('ui/key', { surface_id: open.params.surface_id, key: 'Enter', kind: 'press', modifiers: [] });
  await peer.wait(f => f.method === 'notification' && f.params.message === 'selected:two');
});
test('header/footer/widget placements mount and restore without keeping normal requests open', async t => {
  const peer = launch(t); await peer.init(); const command = peer.command('chrome');
  for (const placement of ['header', 'footer', 'above_editor', 'below_editor']) await peer.wait(f => f.method === 'ui/open' && f.params.placement === placement);
  assert.ok((await command.response).result);
  const clear = peer.command('clear');
  for (let i = 0; i < 4; i++) await peer.wait(f => f.method === 'ui/close');
  assert.ok((await clear.response).result);
});
test('void setters expose host refusal; synchronous read-after-write uses local mirrors', async t => {
  const peer = launch(t); await peer.init();
  assert.equal((await peer.call('mutate').response).result.content[0].text, 'local|renamed');
  const refused = launch(t, undefined, { hold: ['composer/set'] }); await refused.init();
  const call = refused.call('mutate'); const set = await refused.wait(f => f.method === 'composer/set');
  refused.send({ jsonrpc: '2.0', id: set.id, error: { code: -32002, message: 'not_foreground_owner synthetic refusal' } });
  assert.match((await call.response).error.message, /synthetic refusal/);
});
test('ordered async hook callbacks and queued cancellation remain serviceable', async t => {
  const peer = launch(t); await peer.init();
  const invoke = (order, delay) => peer.request('hook/run', { hook: 'before_tool_call', payload: { name: 'ordered', arguments: { order, delay } }, context: peer.context() });
  const first = invoke(1, 80), second = invoke(2, 0), third = invoke(3, 0);
  peer.notify('$/cancelRequest', { id: second.id });
  assert.ok((await first.response).result); assert.equal((await second.response).error.code, -32800); assert.ok((await third.response).result);
  assert.match(peer.stderr(), /ordered:1[\s\S]*ordered:3/); assert.doesNotMatch(peer.stderr(), /ordered:2/);
  assert.equal((await peer.request('hook/run', { hook: 'before_tool_call', payload: { name: 'blocked', arguments: {} }, context: peer.context() }).response).result.disposition.action, 'deny');
});
test('cancelling tools and reverse calls rejects once, ignores late response, and preserves other calls', async t => {
  const peer = launch(t); await peer.init();
  const pending = peer.call('wait'); await peer.wait(f => f.method === '$/progress'); peer.notify('$/cancelRequest', { id: pending.id });
  assert.equal((await pending.response).error.code, -32800);
  const confirm = peer.call('confirm'); const question = await peer.wait(f => f.method === 'confirmation/request');
  peer.notify('$/cancelRequest', { id: confirm.id }); assert.equal((await confirm.response).error.code, -32800);
  peer.send({ jsonrpc: '2.0', id: question.id, result: { confirmed: true } });
  assert.ok((await peer.call('normal').response).result);
});
test('EOF, crash transport and active shutdown are bounded without model/runtime fallback', async t => {
  const peer = launch(t); await peer.init(); const pending = peer.call('wait'); pending.response.catch(() => {}); await peer.wait(f => f.method === '$/progress'); await peer.close();
  const eof = launch(t); const exit = once(eof.child, 'exit'); eof.child.stdin.end(); assert.equal((await exit)[0], 0);
  const bad = launch(t); const badExit = once(bad.child, 'exit'); bad.child.stdin.write(' '.repeat(1048577)); assert.equal((await badExit)[0], 1);
  const malformed = launch(t); const malformedExit = once(malformed.child, 'exit'); malformed.child.stdin.write('{bad json}\n'); assert.equal((await malformedExit)[0], 1);
});
test('unsupported options/results and unnegotiated child calls never silently disappear', async t => {
  const peer = launch(t); await peer.init();
  for (const [mode, reason] of [['unsafe-output', /direct terminal control/], ['runtime', /unsupported_feature agent_sessions: feature was not negotiated/], ['media', /unsupported_feature artifacts/], ['fail', /handler crash/]]) assert.match((await peer.call(mode).response).error.message, reason);
});
test('native chrome refuses calls without a negotiated UI consumer', async t => {
  const peer = launch(t); await peer.init([]);
  assert.match((await peer.call('unsupported').response).error.message, /unsupported_feature remote_ui/);
});
test('shared events preserve synchronous object/function identity across multiple unchanged factories', async t => {
  const dir = await temporary(t), a = join(dir, 'a.ts'), b = join(dir, 'b.ts');
  await writeFile(a, `export default pi => { pi.events.on('identity', object => { object.changed = true; object.fn(); }); };`);
  await writeFile(b, `export default pi => { pi.registerCommand('identity', { handler(_,ctx) { let called=false; const object={changed:false,fn(){called=true}}; pi.events.emit('identity',object); ctx.ui.notify(String(object.changed && called)); } }); };`);
  const peer = launch(t, [a, b]); await peer.init(); await peer.command('identity').response;
  await peer.wait(f => f.method === 'notification' && f.params.message === 'true');
});
test('complete owner fencing rejects foreign updates and closes retained surfaces at session end', async t => {
  const peer = launch(t); await peer.init(); const cmd = peer.command('surface'); await cmd.response; await peer.wait(frame('tick='));
  peer.notify('context/updated', { resource_owner: { ...owner, process_generation: 9 }, host: { model: 'foreign' } });
  await peer.wait(f => f.method === 'notification' && /not_foreground_owner/.test(f.params.message));
  const ended = await peer.request('hook/run', { hook: 'session_end', payload: { binding: owner, reason: 'shutdown', outcome: 'completed', duration_ms: 1 }, context: peer.context() }).response;
  assert.ok(ended.result); assert.match((await peer.call('normal').response).error.message, /settled owner/);
});
test('framing writer honors backpressure, coalesces snapshots, and bounds queues', async () => {
  const callbacks = [], writes = []; let lost;
  const transport = new Transport({ write(line, cb) { writes.push(line); callbacks.push(cb); }, output: {} }, { onLost: e => { lost = e; } });
  const a = transport.send({ a: 1 }); const b = transport.send({ b: 1 }, 'frame'); const c = transport.send({ b: 2 }, 'frame');
  assert.equal(writes.length, 1); callbacks.shift()(); assert.equal(writes.length, 2); assert.match(writes[1], /"b":2/); callbacks.shift()(); await Promise.all([a, b, c]);
  const queued = [];
  for (let i = 0; i < 131; i++) queued.push(transport.send({ i }).catch(() => {}));
  assert.match(lost.message, /writer queue/); callbacks.shift()?.(); await Promise.all(queued);
});
test('safe SGR bounds, release mapping, RGB validation and typed SGR mouse input', () => {
  assert.deepEqual(safeLines(['\x1b[38;2;255;20;0m🙂\x1b[0m']), ['\x1b[38;2;255;20;0m🙂\x1b[0m']);
  for (const line of ['\x1b[2J', '\x1b]8;;https://example.com\x07url', '\x1b[38;2;999;0;0mX', 'a\nb', '\ud800']) assert.throws(() => safeLines([line]));
  assert.throws(() => safeLines(Array(257).fill(''))); assert.throws(() => safeLines(['x'.repeat(16385)]));
  const release = keyData({ key: 'ArrowUp', kind: 'release', modifiers: [] }); assert.ok(isKeyRelease(release)); assert.ok(matchesKey(release, 'up'));
  assert.ok(isKeyRepeat(keyData({ key: 'w', kind: 'repeat', modifiers: ['shift'] })));
  assert.equal(mouseData({ kind: 'drag', button: 'left', x: 2, y: 3, modifiers: ['control'] }), '\x1b[<48;3;4M');
  assert.equal(mouseData({ kind: 'release', button: 'left', x: 2, y: 3 }), '\x1b[<0;3;4m');
});
test('configure captures explicitly reviewed metadata only and never enables/trusts or overwrites implicitly', async t => {
  const dir = await temporary(t), output = join(dir, 'octet-pi-compat');
  const extensions = [join(root, 'test/fixtures/core.ts')];
  assert.throws(() => configure({ output, extensions }), /--reviewed/);
  const result = configure({ output, extensions, reviewed: true }); assert.equal(result.registrations.tools[0].name, 'core');
  const { hookEvents } = await import('../lib/api.mjs');
  // Provider wire and resource hooks are reserved only when captured.
  const captured = inspect(extensions).hooks, wire = new Set(['before_provider_request', 'before_provider_headers', 'after_provider_response', 'resources_discover']);
  const subscribedHooks = [...new Set(Object.values(hookEvents))].filter(h => !wire.has(h) || captured.includes(h)).sort();
  assert.deepEqual(result.registrations.hooks, subscribedHooks);
  const manifest = await readFile(join(output, 'extension.toml'), 'utf8');
  assert.deepEqual(JSON.parse(manifest.match(/^hooks = (\[.*\])$/m)[1]), subscribedHooks);
  assert.doesNotMatch(manifest, /enabled_extensions|trusted_extensions/); assert.deepEqual((await readdir(dir)).sort(), ['octet-pi-compat']);
  assert.throws(() => configure({ output, extensions, reviewed: true }), /exists/);
  const peer = launch(t, extensions, { config: join(output, 'bridge.json') });
  peer.metadata.hooks = result.registrations.hooks; // Admit the reviewed configured catalog, not bare-factory inspection.
  await peer.init(['remote_ui', 'resource_paths_v1', 'session_entries', 'pipeline_hooks_v1']);
  assert.ok((await peer.call('normal').response).result); await peer.close();
});

test('remote overlay composition has real focus, hide/unfocus and component identity', () => {
  let renders = 0, input = '';
  const surface = { columns: 20, rows: 4, requestRender() { renders++; } }, tui = new RemoteTUI(surface);
  const base = { focused: false, render: () => ['base'], handleInput: value => { input = `base:${value}`; } };
  const overlay = { focused: false, render: () => ['overlay'], handleInput: value => { input = `overlay:${value}`; } };
  tui.addChild(base); tui.setFocus(base);
  const handle = tui.showOverlay(overlay, { width: 10, row: 1, col: 2 });
  assert.equal(tui.focus, overlay); assert.ok(handle.isFocused()); tui.input('a', false); assert.equal(input, 'overlay:a');
  assert.match(tui.render(20)[1], /overlay/); handle.setHidden(true); assert.equal(tui.focus, base);
  handle.setHidden(false); assert.equal(tui.focus, overlay); handle.hide(); assert.equal(tui.focus, base); assert.ok(renders > 0);
});

test('production imports and pinned dependency graph never include the Pi coding-agent runtime', async () => {
  const lock = JSON.parse(await readFile(join(root, 'package-lock.json'), 'utf8'));
  assert.deepEqual(Object.keys(lock.packages).sort(), ['', 'node_modules/@earendil-works/pi-tui', 'node_modules/@sinclair/typebox', 'node_modules/get-east-asian-width', 'node_modules/jiti', 'node_modules/marked', 'node_modules/typebox', 'node_modules/yaml'].sort());
  for (const dir of ['lib', 'shims']) for (const file of await readdir(join(root, dir))) {
    if (!file.endsWith('.mjs')) continue;
    const code = await readFile(join(root, dir, file), 'utf8');
    assert.doesNotMatch(code, /(?:from\s*|import\s*\(|require\s*\()['"]@(?:earendil-works|mariozechner)\/pi-coding-agent/);
    assert.doesNotMatch(code, /from\s*['"]@(?:earendil-works|mariozechner)\/pi-tui['"]/);
  }
});

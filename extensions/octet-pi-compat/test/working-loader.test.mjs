import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const component = frame => frame.method === 'ui/chrome' && frame.params.chrome.kind === 'working_component';

test('the working Loader shares extension patches and publishes effective presentation with bounded delivery', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'pi-working-loader-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.mjs');
  writeFileSync(entry, `import { Loader } from '@earendil-works/pi-tui';
    const original = Loader.prototype.updateDisplay;
    Loader.prototype.updateDisplay = function () {
      original.call(this);
      this.setText('\\x1b[31m>>>\\x1b[0m ' + this.message);
    };
    export default pi => {
      pi.on('session_start', (_event, ctx) => ctx.ui.setWorkingMessage('patched message'));
      pi.registerCommand('working', {handler: (args, ctx) => {
        if (args === 'hide') ctx.ui.setWorkingVisible(false);
        else if (args === 'show') ctx.ui.setWorkingVisible(true);
        else ctx.ui.setWorkingMessage(args || undefined);
      }});
    };`);
  const h = launch(t, [entry]);
  await h.init(); await h.start();
  h.notify('turn/started', { resource_owner: owner });
  const first = await h.wait(component, 1500);
  assert.equal(first.params.chrome.line, '\x1b[31m>>>\x1b[0m patched message');
  assert.deepEqual(first.params.resource_owner, owner);
  await delay(260);
  assert.equal(h.seen.filter(component).length, 1, 'one frame in flight even when the host does not answer');
  h.send({jsonrpc: '2.0', id: first.id, result: {tools_expanded: false}});
  const next = await h.wait(component);
  h.send({jsonrpc: '2.0', id: next.id, result: {tools_expanded: false}});
  await h.command('working', ['changed']).response;
  const changed = await h.wait(f => component(f) && f.params.chrome.line?.endsWith('changed'));
  h.send({jsonrpc: '2.0', id: changed.id, result: {tools_expanded: false}});
  h.notify('turn/settled', { resource_owner: owner, outcome: 'cancelled' });
  const stopped = await h.wait(f => component(f) && f.params.chrome.line === null);
  const count = h.seen.filter(component).length;
  h.send({jsonrpc: '2.0', id: stopped.id, result: {tools_expanded: false}});
  await delay(240);
  assert.equal(h.seen.filter(component).length, count, 'stop removes animation timer');
  h.notify('turn/started', { resource_owner: owner });
  const restarted = await h.wait(f => component(f) && f.params.chrome.line?.endsWith('changed'));
  h.send({jsonrpc: '2.0', id: restarted.id, result: {tools_expanded: false}});
  await h.command('working', ['hide']).response;
  const hidden = await h.wait(f => component(f) && f.params.chrome.line === null);
  h.send({jsonrpc: '2.0', id: hidden.id, result: {tools_expanded: false}});
  await h.command('working', ['show']).response;
  const shown = await h.wait(f => component(f) && f.params.chrome.line !== null);
  await h.request('hook/run', {hook: 'session_end', payload: {reason: 'reload'}, context: h.context()}).response;
  const retiredCount = h.seen.filter(component).length;
  h.send({jsonrpc: '2.0', id: shown.id, result: {tools_expanded: false}});
  await delay(240);
  assert.equal(h.seen.filter(component).length, retiredCount, 'late ACK cannot revive a retired owner');
  await h.close();
});

test('Loader indicator reset, verbatim color, theme invalidation and visibility follow Pi lifecycle', async t => {
  const { Runtime } = await import('../lib/runtime.mjs');
  const { workingLoader } = await import('../lib/working-loader.mjs');
  const { foregroundTokens, backgroundTokens } = await import('../lib/theme-palette.mjs');
  const frames = [];
  const runtime = new Runtime({extensions: []}, {request: async (_method, params) => { frames.push(params.chrome.line); return {}; }});
  runtime.features.add('remote_ui');
  const palette = color => ({name: 'test', path: null, appearance: 'dark',
    colors: Object.fromEntries([...foregroundTokens, ...backgroundTokens].map(token => [token, color])),
    foregrounds: Object.fromEntries(foregroundTokens.map(token => [token, {color, dim: false}])),
    backgrounds: Object.fromEntries(backgroundTokens.map(token => [token, color])),
    capabilities: {color: 'truecolor', bold: true, dim: true, italic: true, underline: true, inverse: true, strikethrough: true}});
  const state = {parent: 9, alive: true, owner, host: {theme: palette('#102030')}, runActive: true};
  runtime.foreground = state;
  const working = workingLoader(runtime, state);
  t.after(() => working.retire());
  working.indicator({frames: ['\x1b[32mMULTI\x1b[0m'], intervalMs: 7});
  working.sync(); await delay(25);
  assert.equal(working.loader.intervalId, null, 'single frame does not install a timer');
  assert.match(frames.at(-1), /^\x1b\[32mMULTI\x1b\[0m /, 'explicit frames are verbatim');
  assert.match(frames.at(-1), /16;32;48/);
  state.host.theme = palette('#abcdef'); working.invalidate(); await delay(25);
  assert.match(frames.at(-1), /171;205;239/);
  assert.match(frames.at(-1), /^\x1b\[32mMULTI/, 'theme does not recolor explicit frames');
  working.indicator({frames: []}); await delay(25);
  assert.equal(working.loader.intervalId, null);
  assert.equal(frames.at(-1).replace(/\x1b\[[0-9;]*m/g, ''), 'Working');
  working.indicator(); await delay(25);
  assert.ok(working.loader.intervalId, 'reset restores upstream animation');
  assert.match(frames.at(-1), /171;205;239m⠋/);
  const { Loader } = await import('../node_modules/@earendil-works/pi-tui/dist/components/loader.js');
  const dialog = new Loader({requestRender() {}}, text => text, text => text, 'UNRELATED DIALOG');
  t.after(() => dialog.stop());
  await delay(25);
  assert.ok(frames.every(line => !line?.includes('UNRELATED DIALOG')), 'dialog Loaders do not feed the working row');
  dialog.stop();
  const previous = working.loader;
  working.visibility(false);
  assert.equal(previous.intervalId, null);
  assert.equal(working.loader, undefined);
  working.visibility(true); await delay(25);
  assert.notEqual(working.loader, previous, 'show starts a fresh Loader at frame zero');
  assert.match(frames.at(-1), /m⠋/);
  working.indicator({frames: ['STATIC']});
  const oldRender = working.loader.ui.requestRender;
  working.visibility(false); working.visibility(true);
  oldRender(); // A retired callback must not suppress a newer static component.
  await delay(25);
  assert.match(frames.at(-1), /^STATIC /);
  state.runActive = false; working.sync(); await delay(25);
  assert.equal(frames.at(-1), null);
  const count = frames.length;
  working.retire(); previous.ui.requestRender(); await delay(25);
  assert.equal(frames.length, count, 'retired render callbacks cannot publish');
});

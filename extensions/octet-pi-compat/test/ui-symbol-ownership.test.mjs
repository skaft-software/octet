import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';
import { facade } from '../lib/errors.mjs';

test('shared UI Symbol metadata retains ordinary reads, assignment and deletion without sharing methods', () => {
  const symbols = new Map(), key = Symbol.for('extension.footer-owner');
  const first = facade({ method: () => 'first' }, 'ctx.ui', symbols);
  const second = facade({ method: () => 'second' }, 'ctx.ui', symbols);
  first[key] = { token: true };
  assert.equal(second[key], first[key]);
  assert.equal(key in second, true);
  assert.ok(Reflect.ownKeys(second).includes(key));
  assert.equal(Object.getOwnPropertyDescriptor(second, key).value, first[key]);
  assert.equal(first.method(), 'first'); assert.equal(second.method(), 'second');
  delete second[key];
  assert.equal(first[key], undefined); assert.equal(key in first, false);
  assert.throws(() => { second.unknownAction = () => {}; }, /unsupported_feature/);
  const independent = facade({}, 'ctx.ui', new Map());
  first[key] = 'old'; assert.equal(independent[key], undefined);
});

test('footer ownership survives callback boundaries, clears the actual slot and cannot cross owners', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'octet-ui-symbols-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const entry = join(directory, 'ownership.mjs');
  await writeFile(entry, `const key = Symbol.for('extension.footer-owner');
let captured, installed;
export default pi => {
  const install = ctx => {
    if (ctx.ui[key] !== undefined) throw Error('new owner inherited old metadata');
    captured ??= ctx; installed = Symbol('owned'); ctx.ui[key] = installed;
    ctx.ui.setFooter(() => ({render: () => ['OWNED FOOTER'], invalidate(){}}));
  };
  pi.on('session_start', (_, ctx) => install(ctx));
  pi.registerCommand('clear', {handler: (_, ctx) => {
    if (ctx.ui[key] !== installed) throw Error('ownership vanished between callbacks');
    ctx.ui.setFooter(undefined); delete ctx.ui[key];
  }});
  pi.registerCommand('check-clear', {handler: (_, ctx) => {
    if (ctx.ui[key] !== undefined) throw Error('ownership deletion was not shared');
    ctx.ui.notify('symbol cleared');
  }});
  pi.registerCommand('stale', {handler: () => captured.ui.setFooter(() => ({render: () => ['STALE FOOTER'],invalidate(){}}))});
};`);
  const peer = launch(t, [entry]); await peer.init(); await peer.start();
  const opened = await peer.wait(frame => frame.method === 'ui/open' && frame.params.placement === 'footer');
  await peer.wait(frame => frame.method === 'ui/frame' && frame.params.lines.includes('OWNED FOOTER'));
  const cleared = await peer.command('clear').response;
  assert.ok(cleared.result, JSON.stringify(cleared));
  await peer.wait(frame => frame.method === 'ui/close' && frame.params.surface_id === opened.params.surface_id);
  assert.ok((await peer.command('check-clear').response).result);
  await peer.wait(frame => frame.method === 'notification' && frame.params.message === 'symbol cleared');
  const replacementOwner = { ...owner, session_id: 'replacement-owner' };
  const context = { ...peer.context(), resource_owner: replacementOwner };
  const replaced = await peer.request('hook/run', { hook: 'session_start', payload: { binding: replacementOwner }, context }).response;
  assert.ok(replaced.result, JSON.stringify(replaced));
  await peer.wait(frame => frame.method === 'ui/open' && frame.params.resource_owner.session_id === replacementOwner.session_id);
  const stale = await peer.request('command/execute', { name: 'stale', arguments: [], context }).response;
  assert.ok(stale.error, JSON.stringify(stale));
  assert.equal(peer.seen.some(frame => frame.method === 'ui/frame' && frame.params.lines.includes('STALE FOOTER')), false);
  await peer.close();
});

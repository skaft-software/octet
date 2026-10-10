import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, writeFile, mkdtemp, mkdir, rm, realpath, symlink } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { configure } from '../configure.mjs';
import { normalizeResourcePath } from '../lib/resources.mjs';
import { root, launch as launchAdapter, owner } from './helper.mjs';
import { foregroundTokens, backgroundTokens } from '../lib/theme-palette.mjs';

// Discovery now applies to ordinary bridges too. Never inventory the real
// user's Pi home in synthetic resource-contract tests.
function launch(t, extensions, options = {}) {
  return launchAdapter(t, extensions, { ...options, env: { OCTET_PI_AGENT_DIR: join(root, 'test/fixtures/no-pi-agent'), ...options.env } });
}
const palette = name => ({ name, appearance: 'dark', colors: Object.fromEntries([
  ...foregroundTokens.map(token => [token, '#ffffff']), ...backgroundTokens.map(token => [token, '']),
]) });

const fixture = join(root, 'test/fixtures/resources.ts'), second = join(root, 'test/fixtures/resources-second.ts');
const empty = { resource_paths: { skill_paths: [], prompt_paths: [], theme_paths: [] } };
const discover = (peer, reason = 'startup', payload = {}, context = peer.context()) => peer.request('hook/run', {
  hook: 'resources_discover', payload: { cwd: context.workspace, reason, ...payload }, context,
});
async function state(peer, args = {}, context = peer.context()) {
  const reply = await peer.request('tool/call', { name: 'resource_state', arguments: args, context }).response;
  assert.ok(reply.result, JSON.stringify(reply)); return JSON.parse(reply.result.content[0].text);
}
const set = (peer, config) => state(peer, { configure: config });
const labels = value => value.calls.map(call => call.label);
async function started(t, options = {}, extensions = [fixture]) {
  const peer = launch(t, extensions, options); await peer.init(['resource_paths_v1', 'session_entries']); await peer.start(); return peer;
}
async function directory(t) {
  const dir = await realpath(await mkdtemp(join(tmpdir(), 'octet-resource-paths-')));
  t.after(() => rm(dir, { recursive: true, force: true })); return dir;
}

test('resources: nonempty ordinary factories return the exact native envelope, ordered paths and actual startup/reload facts', async t => {
  const cwd = await directory(t);
  await mkdir(join(cwd, 'skills')); await mkdir(join(cwd, 'more-skills')); await mkdir(join(cwd, 'prompts'));
  await writeFile(join(cwd, 'skills/SKILL.md'), '---\nname: discovered\ndescription: Ordinary positive fixture\n---\nSkill body sentinel\n');
  await writeFile(join(cwd, 'prompts/sentinel.md'), 'Prompt sentinel $1\n');
  await writeFile(join(cwd, 'more-skills/SKILL.md'), '---\nname: more-discovered\ndescription: Another ordinary fixture\n---\nAnother skill body\n');
  await writeFile(join(cwd, 'theme.toml'), '[metadata]\nname = "Resource Proof"\n[glyphs]\nprompt = ":"\n');
  const peer = await started(t, { cwd }, [fixture, second]);
  const paths = { resource_paths: { skill_paths: [join(cwd, 'skills'), join(cwd, 'more-skills'), join(cwd, 'skills')],
    prompt_paths: [join(cwd, 'prompts')], theme_paths: [join(cwd, 'theme.toml')] } };
  for (const reason of ['startup', 'reload']) {
    await set(peer, {});
    assert.deepEqual((await discover(peer, reason).response).result, paths);
    const observed = await state(peer); assert.deepEqual(labels(observed), ['first', 'middle', 'last']);
    for (const call of observed.calls) assert.deepEqual(call, { label: call.label, type: 'resources_discover', cwd, reason, contextCwd: cwd });
  }
  // Existence is not parsing/loading qualification; only this real subprocess reply is established.
  await peer.close();
});

test('resources: actual wire paths resolve against event cwd with Pi trim, home, file-URL and Unicode semantics', async t => {
  const cwd = await directory(t), peer = await started(t, { cwd });
  const url = pathToFileURL(join(cwd, 'file url 前😀')).href;
  await set(peer, { results: { first: { skillPaths: ['\t ./skills\n', '../shared', '~', ' ~/prompts ', url],
    promptPaths: ['', 'inner\u00a0space/e\u0301', '@literal'], themePaths: ['./themes/native.toml'] } } });
  assert.deepEqual((await discover(peer).response).result, { resource_paths: {
    skill_paths: [join(cwd, 'skills'), resolve(cwd, '../shared'), homedir(), join(homedir(), 'prompts'), join(cwd, 'file url 前😀')],
    prompt_paths: [cwd, join(cwd, 'inner\u00a0space/e\u0301'), join(cwd, '@literal')], theme_paths: [join(cwd, 'themes/native.toml')],
  } });
  await peer.close();
});

test('resources: handler snapshot survives removal and each callback receives a fresh native event', async t => {
  const peer = await started(t, {}, [fixture, second]);
  await set(peer, { mode: 'remove' }); assert.ok((await discover(peer).response).result);
  assert.deepEqual(labels(await state(peer)), ['first', 'middle', 'last']);
  await set(peer, { mode: 'mutate-event' }); const result = (await discover(peer, 'reload').response).result;
  assert.deepEqual(labels(await state(peer)), ['first', 'last']);
  const last = (await state(peer)).calls.at(-1);
  assert.equal(last.cwd, root); assert.equal(last.reason, 'reload');
  assert.deepEqual(result.resource_paths.skill_paths, [join(root, 'skills'), join(root, 'skills')]);
  await peer.close();
});

test('resources: aggregation awaits a real async callback before invoking later factories or replying', async t => {
  const peer = await started(t, {}, [fixture, second]); await set(peer, { mode: 'hold' });
  const pending = discover(peer); await state(peer, { wait_for: 'first' });
  assert.deepEqual(labels(await state(peer)), ['first']);
  assert.equal(peer.seen.some(f => f.id === pending.id && !f.method), false);
  await state(peer, { release: 'first' });
  assert.deepEqual((await pending.response).result.resource_paths.skill_paths, [join(root, 'skills'), join(root, 'more-skills'), join(root, 'skills')]);
  assert.deepEqual(labels(await state(peer)), ['first', 'middle', 'last']); await peer.close();
});

test('resources: ordered hook queue waits for session_start; queued cancellation is prompt and does not execute discovery', async t => {
  const peer = launch(t, [fixture]); await peer.init(['resource_paths_v1']); await set(peer, { mode: 'hold-start' });
  const start = peer.request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: peer.context() });
  await state(peer, { wait_for: 'session_start' });
  const pending = discover(peer); assert.deepEqual(labels(await state(peer)), ['session_start']);
  peer.notify('$/cancelRequest', { id: pending.id }); assert.equal((await pending.response).error.code, -32800);
  assert.deepEqual(labels(await state(peer)), ['session_start']);
  assert.equal(peer.seen.some(f => f.id === start.id && !f.method), false);
  await state(peer, { release: 'session_start' }); assert.ok((await start.response).result);
  assert.ok((await discover(peer).response).result); assert.deepEqual(labels(await state(peer)), ['session_start', 'first', 'middle']);
  await peer.close();
});

test('resources: empty and undefined contributions are genuine empty replacements, not stale cached paths', async t => {
  const peer = await started(t); assert.ok((await discover(peer).response).result.resource_paths.skill_paths.length);
  for (const config of [{ mode: 'empty' }, { results: {} }, { results: { first: { skillPaths: [], promptPaths: [], themePaths: [] } } }]) {
    await set(peer, config); assert.deepEqual((await discover(peer, 'reload').response).result, empty);
  }
  await peer.close();
});

test('resources: ordinary callback failures are diagnosed with factory provenance; contract refusals are not swallowed', async t => {
  const peer = await started(t, {}, [fixture, second]);
  await set(peer, { mode: 'throw' });
  assert.deepEqual((await discover(peer).response).result.resource_paths.skill_paths, [join(root, 'more-skills'), join(root, 'skills')]);
  const diagnostic = await peer.wait(f => f.method === 'notification' && f.params.title === '[Extension issues]');
  assert.ok(diagnostic.params.message.includes(fixture)); assert.match(diagnostic.params.message, /resources_discover callback failed/);
  assert.match(diagnostic.params.message, /Next:/);
  assert.doesNotMatch(diagnostic.params.message, /ordinary discovery callback failure/);
  assert.deepEqual(labels(await state(peer)), ['first', 'middle', 'last']);
  await set(peer, { mode: 'unsupported' });
  assert.match((await discover(peer).response).error.message, /unsupported_feature ctx.abort/);
  assert.deepEqual(labels(await state(peer)), ['first']); await peer.close();
});

test('resources: owner-bound reverse calls preserve the real parent, await ACK, and propagate host refusal', async t => {
  const peer = await started(t, { hold: ['session/set_name'] }); await set(peer, { mode: 'reverse' });
  for (const refuse of [false, true]) {
    const pending = discover(peer);
    const reverse = await peer.wait(f => f.method === 'session/set_name');
    assert.equal(reverse.params.parent_request_id, pending.id); assert.deepEqual(reverse.params.resource_owner, owner);
    assert.equal(peer.seen.some(f => f.id === pending.id && !f.method), false);
    peer.send({ jsonrpc: '2.0', id: reverse.id, ...(refuse ? { error: { code: -32019, message: 'host refused test reverse call' } } : { result: {} }) });
    const response = await pending.response;
    if (refuse) assert.equal(response.error.code, -32019); else assert.ok(response.result);
  }
  await peer.close();
});

test('resources: actual callback cancellation releases the hook queue and suppresses late results exactly once', async t => {
  const peer = await started(t); await set(peer, { mode: 'hold' });
  const pending = discover(peer); await state(peer, { wait_for: 'first' });
  peer.notify('$/cancelRequest', { id: pending.id }); assert.equal((await pending.response).error.code, -32800);
  assert.deepEqual((await state(peer)).aborted, ['first']); assert.deepEqual(labels(await state(peer)), ['first']);
  // Prove next hook completes while the cancelled trusted callback is still unresolved.
  await set(peer, { mode: 'empty' }); assert.deepEqual((await discover(peer, 'reload').response).result, empty);
  await state(peer, { release: 'first' }); const after = await state(peer);
  assert.deepEqual(after.resumed, ['first']);
  assert.equal(peer.seen.filter(f => f.id === pending.id && !f.method).length, 1);
  assert.equal(peer.seen.some(f => f.id === pending.id && f.result), false); await peer.close();
});

for (const changed of ['session_id', 'extension_instance_id', 'process_generation']) test(`resources: ${changed} replacement aborts discovery and rejects retired-owner reuse`, async t => {
  const peer = await started(t); await set(peer, { mode: 'hold' });
  const pending = discover(peer); await state(peer, { wait_for: 'first' });
  const context = { ...peer.context(), resource_owner: { ...owner, [changed]: changed === 'process_generation' ? owner.process_generation + 1 : `replacement-${changed}` } };
  // Only the native lifecycle binding authorizes a foreground replacement.
  assert.ok((await peer.request('hook/run', { hook: 'session_start', payload: { binding: context.resource_owner }, context }).response).result);
  const observed = await state(peer, {}, context); assert.deepEqual(observed.aborted, ['first']);
  assert.equal((await pending.response).error.code, -32800);
  await state(peer, { release: 'first' }, context); await state(peer, {}, context);
  assert.equal(peer.seen.filter(f => f.id === pending.id && !f.method).length, 1);
  assert.match((await discover(peer).response).error.message, /settled owner/);
  await state(peer, { configure: { mode: 'empty' } }, context);
  assert.deepEqual((await discover(peer, 'reload', {}, context).response).result, empty); await peer.close();
});

test('resources: session_end prevents reuse of the same owner for discovery', async t => {
  const peer = await started(t);
  assert.ok((await peer.request('hook/run', { hook: 'session_end', payload: { binding: owner }, context: peer.context() }).response).result);
  assert.match((await discover(peer).response).error.message, /settled owner/); await peer.close();
});

test('resources: malformed input and missing native context owners are refused before callbacks', async t => {
  const peer = await started(t); await set(peer, {});
  for (const payload of [null, {}, { cwd: root, reason: 'invented' }, { cwd: '.', reason: 'startup' }, { cwd: root, reason: 'startup', extra: true }, { cwd: '/bad\n', reason: 'startup' }]) {
    const result = await peer.request('hook/run', { hook: 'resources_discover', payload, context: peer.context() }).response;
    assert.ok(result.error, JSON.stringify(payload));
  }
  for (const resource_owner of [undefined, { session_id: 'incomplete' }, { ...owner, process_generation: -1 }]) {
    const context = { ...peer.context(), resource_owner };
    assert.match((await discover(peer, 'startup', {}, context).response).error.message, /resource_owner/);
  }
  const noContext = await peer.request('hook/run', { hook: 'resources_discover', payload: { cwd: root, reason: 'startup', binding: owner } }).response;
  assert.ok(noContext.error); assert.deepEqual(labels(await state(peer)), []); await peer.close();
});

test('resources: malformed callback values and unknown fields fail the entire response without silent drops', async t => {
  const peer = await started(t);
  for (const first of [null, [], 'not-an-object', 1, { skillPaths: null }, { skillPaths: 'path' }, { skillPaths: [1] },
    { skillPaths: [null] }, { skillPaths: ['/bad\u0000'] }, { skillPaths: ['/bad\npart'] }, { skillPaths: ['/bad\tpart'] },
    { skillPaths: ['/bad\u0085part'] }, { skillPaths: ['\ud800'] }, { skillPaths: ['file:///bad%00path'] },
    { skillPaths: ['file://remotehost/path'] }, { skillPaths: ['builtin:skill'] }, { themePaths: ['<inline:theme>'] },
    { resource_paths: {} }, { skillPaths: ['./skills'], context: [] }, { block: true }]) {
    await set(peer, { results: { first } }); const reply = await discover(peer).response;
    assert.ok(reply.error, JSON.stringify(first)); assert.equal(reply.result, undefined);
    assert.ok(reply.error.message.includes(fixture)); assert.deepEqual(labels(await state(peer)), ['first']);
  }
  for (const mode of ['sparse', 'array-extra', 'array-symbol', 'symbol', 'hidden', 'inherited', 'date']) {
    await set(peer, { mode }); assert.ok((await discover(peer).response).error, mode);
  }
  await peer.close();
});

test('resources: bounds count normalized UTF-8 bytes and all arrays/handlers before deduplication', async t => {
  const peer = await started(t);
  const exact = '/' + 'é'.repeat(2047) + 'x'; assert.equal(Buffer.byteLength(exact), 4096);
  await set(peer, { results: { first: { skillPaths: Array(64).fill('/x') } } });
  assert.equal((await discover(peer).response).result.resource_paths.skill_paths.length, 64);
  for (const results of [
    { first: { skillPaths: Array(65).fill('/x') } },
    { first: { skillPaths: Array(64).fill('/x'), promptPaths: ['/x'] } },
    { first: { skillPaths: Array(64).fill('/x') }, middle: { themePaths: ['/x'] } },
    { first: { skillPaths: [exact + 'x'] } },
    { first: { skillPaths: ['x'.repeat(4096)] } }, // Input fits; resolved cwd + path does not.
    { first: { skillPaths: Array(16).fill(exact), themePaths: ['/x'] } },
    { first: { skillPaths: Array(8).fill(exact) }, middle: { promptPaths: Array(9).fill(exact) } },
  ]) {
    await set(peer, { results }); assert.match((await discover(peer).response).error.message, /bounds_exceeded/);
  }
  await set(peer, { results: { first: { skillPaths: Array(8).fill(exact) }, middle: { promptPaths: Array(8).fill(exact) } } });
  const paths = (await discover(peer).response).result.resource_paths;
  assert.equal(paths.skill_paths.length, 8); assert.equal(paths.prompt_paths.length, 8);
  assert.equal([...paths.skill_paths, ...paths.prompt_paths].reduce((n, path) => n + Buffer.byteLength(path), 0), 65536);
  await peer.close();
});

test('resources: configure captures declarations without discovery, requires negotiated consumer, and preserves exact catalog equality', async t => {
  const dir = await directory(t), output = join(dir, 'octet-pi-compat');
  const entry = join(dir, 'capture.ts');
  await writeFile(entry, `export default pi => { pi.on('resources_discover', () => { throw new Error('capture must not invoke discovery'); }); };\n`);
  const { registrations } = configure({ reviewed: true, output, piAgentDir: join(dir, 'agent'), extensions: [entry] });
  const { hookEvents } = await import('../lib/api.mjs');
  const wire = new Set(['before_provider_request', 'before_provider_headers', 'after_provider_response']); // reserved only when a factory registered them
  const subscribedHooks = [...new Set(Object.values(hookEvents))].filter(h => !wire.has(h)).sort();
  const subscribedFeatures = ['resource_paths_v1', 'session_entries', 'pipeline_hooks_v1'];
  assert.deepEqual(registrations.hooks, subscribedHooks); assert.deepEqual(registrations.events, ['resources_discover']);
  const manifest = await readFile(join(output, 'extension.toml'), 'utf8');
  assert.deepEqual(JSON.parse(manifest.match(/^hooks = (\[.*\])$/m)[1]), subscribedHooks);
  assert.doesNotMatch(manifest, /resource_paths_v1|skillPaths|resource_paths\s*=/);
  const config = join(output, 'bridge.json'), peer = launch(t, [entry], { config });
  await assert.rejects(peer.init([]), /unsupported_feature resource_paths_v1/); await peer.close();
  const configured = launch(t, [entry], { config });
  configured.metadata.hooks = registrations.hooks;
  assert.ok((await configured.init(subscribedFeatures)).protocol.features.includes('resource_paths_v1'));
  assert.equal(configured.seen.some(f => f.method === 'notification'), false, 'capture/initialize did not execute discovery');
  await configured.close();
  const missingHook = launch(t, [entry], { config });
  const mismatch = await missingHook.request('initialize', { api_version: '0.4', workspace: dir, contributes: {},
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: subscribedFeatures },
  }).response;
  assert.match(mismatch.error.message, /manifest hooks differs from reviewed registrations/); await missingHook.close();
  const old = JSON.parse(await readFile(config, 'utf8')); old.registrations.events = [];
  await writeFile(config, JSON.stringify(old));
  const changed = launch(t, [entry], { config });
  changed.metadata.hooks = registrations.hooks;
  await assert.rejects(changed.init(subscribedFeatures), /reviewed registration metadata changed/); await changed.close();
  const undeclaredEntry = join(dir, 'none.ts'); await writeFile(undeclaredEntry, 'export default pi => {};\n');
  configure({ reviewed: true, output, piAgentDir: join(dir, 'agent'), extensions: [undeclaredEntry], overwrite: true });
  const undeclared = launch(t, [undeclaredEntry], { config });
  undeclared.metadata.hooks = subscribedHooks;
  await undeclared.init(subscribedFeatures); await undeclared.start();
  assert.deepEqual((await discover(undeclared).response).result, empty); await undeclared.close();
});

test('resources: configured ordinary bridges reserve the real palette consumer and permit late resource subscriptions', async t => {
  const dir = await directory(t), output = join(dir, 'octet-pi-compat');
  const entry = join(root, 'test/fixtures/core.ts');
  const { registrations } = configure({ reviewed: true, output, piAgentDir: join(dir, 'agent'), extensions: [entry] });
  assert.equal(registrations.hooks.includes('resources_discover'), true);
  const peer = launch(t, [entry], { config: join(output, 'bridge.json') });
  peer.metadata.hooks = registrations.hooks;
  const initialized = await peer.init(['session_entries', 'resource_paths_v1']);
  assert.equal(initialized.protocol.features.includes('resource_paths_v1'), true);
  assert.equal((await peer.call('ordinary').response).result.content[0].text, `${root}|false|host-value`);
  await peer.close();

  const lateEntry = join(dir, 'late.ts');
  await writeFile(lateEntry, `export default pi => {
    pi.registerCommand('late', { handler() { pi.on('resources_discover', () => ({ skillPaths: ['./skills'] })); } });
  };\n`);
  const lateCapture = configure({ reviewed: true, output, piAgentDir: join(dir, 'agent'), extensions: [lateEntry], overwrite: true });
  const late = launch(t, [lateEntry], { config: join(output, 'bridge.json') });
  late.metadata.hooks = lateCapture.registrations.hooks;
  // The generated manifest already reserves the real consumer, without
  // inventing a factory callback or adding an event to its reviewed catalog.
  assert.deepEqual(lateCapture.registrations.events, []);
  await late.init(['resource_paths_v1', 'session_entries', 'pipeline_hooks_v1']);
  await late.start();
  assert.ok((await late.command('late').response).result);
  assert.deepEqual((await discover(late).response).result.resource_paths.skill_paths, [join(root, 'skills')]);
  await late.close();
});

for (const mirror of [false, true]) test(`resources: ${mirror ? 'mirror' : 'manual non-mirror'} palettes refresh before a full skill inventory within the shared budget`, async t => {
  const cwd = await directory(t), agent = join(cwd, 'agent'), output = join(cwd, 'octet-pi-compat');
  await mkdir(join(agent, 'themes'), { recursive: true });
  const first = join(agent, 'themes/first.json'), second = join(agent, 'themes/second.json'), third = join(agent, 'themes/third.json');
  await writeFile(first, JSON.stringify(palette('first'))); await writeFile(second, JSON.stringify(palette('second')));
  await writeFile(join(agent, 'settings.json'), JSON.stringify({ theme: 'first' }));
  await writeFile(join(agent, 'keybindings.json'), '{}'); await writeFile(join(agent, 'AGENTS.md'), 'fixture context');
  for (let i = 0; i < 64; i++) {
    const skill = join(agent, `skills/skill-${String(i).padStart(2, '0')}`);
    await mkdir(skill, { recursive: true }); await writeFile(join(skill, 'SKILL.md'), '# skill');
  }
  const entry = join(cwd, 'empty.mjs'); await writeFile(entry, 'export default pi => {};');
  const { registrations: metadata } = configure({ reviewed: true, output, extensions: [entry], piAgentDir: agent, mirrorPiSetup: mirror, cwd });
  const peer = launch(t, [entry], { config: join(output, 'bridge.json'), metadata, cwd });
  await peer.init(['resource_paths_v1', 'session_entries']); await peer.start();
  const startup = (await discover(peer).response).result.resource_paths;
  assert.deepEqual(startup.theme_paths, [first, second]);
  assert.equal(startup.default_theme, mirror ? first : undefined);
  assert.equal(startup.skill_paths.length, mirror ? 60 : 0);
  assert.equal(Object.entries(startup).filter(([key]) => key.endsWith('_paths')).reduce((n, [, paths]) => n + paths.length, 0), mirror ? 64 : 2);
  await rm(second); await writeFile(third, JSON.stringify(palette('third')));
  const reload = (await discover(peer, 'reload').response).result.resource_paths;
  assert.deepEqual(reload.theme_paths, [first, third]);
  assert.equal(reload.default_theme, mirror ? first : undefined);
  if (mirror) await peer.waitStderr(/Pi skills: .*shared 64-path/);
  await peer.close();
});

test('resources: live palettes take priority over old unselected import snapshots, never the selected snapshot', async t => {
  const cwd = await directory(t), agent = join(cwd, 'agent'), output = join(cwd, 'octet-pi-compat');
  await mkdir(join(agent, 'themes'), { recursive: true }); await mkdir(join(cwd, 'snapshots'));
  const live = [], snapshots = [];
  for (let i = 0; i < 50; i++) {
    const path = join(agent, `themes/live-${String(i).padStart(2, '0')}.json`);
    await writeFile(path, JSON.stringify(palette(`live-${i}`))); live.push(path);
  }
  for (let i = 0; i < 20; i++) {
    const path = join(cwd, `snapshots/pi-old-${String(i).padStart(2, '0')}.toml`);
    await writeFile(path, `[metadata]\nname = "pi-old-${i}"\n`); snapshots.push(path);
  }
  const selected = snapshots.at(-1), entry = join(cwd, 'empty.mjs'); await writeFile(entry, 'export default pi => {};');
  await writeFile(join(cwd, 'selected.json'), JSON.stringify(palette('old-19')));
  const { registrations: metadata } = configure({ reviewed: true, output, extensions: [entry], piAgentDir: agent, cwd,
    themePaths: snapshots, piTheme: { name: 'old-19', path: join(cwd, 'selected.json'), selector: 'pi-old-19', nativePath: selected } });
  const peer = launch(t, [entry], { config: join(output, 'bridge.json'), metadata, cwd });
  await peer.init(['resource_paths_v1', 'session_entries']); await peer.start();
  const paths = (await discover(peer).response).result.resource_paths;
  assert.deepEqual(paths.theme_paths, [...live, ...snapshots.slice(0, 13), selected]);
  assert.equal(paths.default_theme, selected); assert.equal(paths.theme_paths.length, 64);
  await peer.waitStderr(/Pi themes: 6 paths exceeded/); await peer.close();
});

test('resources: safe exact palette files preserve malformed JSON for native validation with bounded filesystem diagnostics', async t => {
  const cwd = await directory(t), agent = join(cwd, 'agent'), output = join(cwd, 'octet-pi-compat');
  await mkdir(join(agent, 'themes'), { recursive: true });
  const valid = join(agent, 'themes/valid.json'), broken = join(agent, 'themes/broken.json');
  await writeFile(valid, JSON.stringify(palette('valid'))); await writeFile(broken, '{');
  await writeFile(join(agent, 'themes/000-oversized.json'), 'x'.repeat(256 * 1024 + 1));
  for (let i = 0; i < 40; i++) await symlink(valid, join(agent, `themes/a-linked-${i}.json`));
  const entry = join(cwd, 'empty.mjs'); await writeFile(entry, 'export default pi => {};');
  const { registrations: metadata } = configure({ reviewed: true, output, extensions: [entry], piAgentDir: agent, cwd });
  const peer = launch(t, [entry], { config: join(output, 'bridge.json'), metadata, cwd });
  await peer.init(['resource_paths_v1', 'session_entries']); await peer.start();
  // Real adapter wire evidence only, not selector/schema qualification. Rust
  // owns validation after resolving winning roots, including malformed files.
  assert.deepEqual((await discover(peer).response).result.resource_paths.theme_paths, [broken, valid]);
  await peer.waitStderr(/further discovery diagnostics omitted/);
  const diagnostics = peer.stderr().split('\n').filter(line => line.startsWith('[pi-compat] Pi setup'));
  assert.equal(diagnostics.length, 33);
  assert.ok(diagnostics.some(line => /symlinks/.test(line)));
  assert.ok(diagnostics.some(line => /mirror limit/.test(line)));
  await peer.close();
});

test('resources: palette budget retains a malformed later-stem winner instead of exposing its shadow', async t => {
  const cwd = await directory(t), agent = join(cwd, 'agent'), output = join(cwd, 'octet-pi-compat');
  await mkdir(join(agent, 'themes'), { recursive: true });
  await mkdir(join(cwd, '.pi/themes'), { recursive: true });
  // Pi discovery returns project paths before user paths; native admission
  // retains its documented later-root precedence without validating JSON here.
  const lower = join(cwd, '.pi/themes/p-00.json'), winner = join(agent, 'themes/p-00.json');
  for (let i = 0; i < 64; i++) {
    await writeFile(join(cwd, `.pi/themes/p-${String(i).padStart(2, '0')}.json`), JSON.stringify(palette(`p-${i}`)));
  }
  await writeFile(winner, '{invalid winning JSON');
  const entry = join(cwd, 'empty.mjs'); await writeFile(entry, 'export default pi => {};');
  const { registrations: metadata } = configure({ reviewed: true, output, extensions: [entry], piAgentDir: agent, cwd });
  const peer = launch(t, [entry], { config: join(output, 'bridge.json'), metadata, cwd });
  await peer.init(['resource_paths_v1', 'session_entries']); await peer.start({ project_trusted: true });
  const paths = (await discover(peer, 'startup', {}, peer.context({ project_trusted: true })).response).result.resource_paths.theme_paths;
  assert.equal(paths.length, 64);
  assert.ok(paths.includes(winner), 'native loader must validate the winning JSON, never a fallback');
  assert.ok(!paths.includes(lower), 'resource budget cannot resurrect the shadowed lower root');
  await peer.waitStderr(/Pi setup has 65 themes/);
  await peer.close();
});

const repo = process.env.PI_REFERENCE_REPO;
test('resources: normalization matches source-extracted, hash-verified Pi 1.0.2 resolvePath with resource trim semantics', {
  skip: !repo && 'set PI_REFERENCE_REPO to the local reviewed Pi reference checkout',
}, async () => {
  const source = path => execFileSync('git', ['-C', repo, 'show', `cd32f7725fdbddbaecdff5b1e68491563394e0ca:packages/coding-agent/src/${path}`], { encoding: 'utf8', timeout: 10000 });
  const paths = source('utils/paths.ts');
  assert.equal(createHash('sha256').update(paths).digest('hex'), '64c3ebef724fa21ed0042e127908b4323a5d2f1b8aaa91eee550442332fe8502');
  const loader = source('core/resource-loader.ts');
  assert.equal(createHash('sha256').update(loader).digest('hex'), '45daa19ad4aa08856cf1eb9b026230b918eb25e163c7dc9ab5cf8add6825955a');
  assert.ok(loader.includes('resolvePath(p, this.cwd, { trim: true })'));
  const pure = paths.slice(paths.indexOf('export function normalizeWindowsShellPath'), paths.indexOf('export function getCwdRelativePath'));
  const js = `import { homedir } from 'node:os'; import { isAbsolute, join, resolve as nodeResolvePath } from 'node:path'; import { fileURLToPath } from 'node:url';\nconst UNICODE_SPACES = /[\\u00A0\\u2000-\\u200A\\u202F\\u205F\\u3000]/g;\n` + stripTypeScriptTypes(pure);
  const { resolvePath } = await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);
  const inputs = ['', ' ', '.', '..', './skills', '../shared/./skills/', ' a/../skills ', '\t ./skills\n', '~', ' ~/skills ', '~other/skills', '@skills',
    '前😀/é', 'e\u0301/资源', '\u3000./skills\u00a0', 'inner\u00a0space/skills', '/tmp//skills/../prompts', '//tmp/skills',
    pathToFileURL(join(tmpdir(), 'file url 前😀/skills')).href, 'file:///tmp/a%20b/../skills', 'file:///tmp/encoded%2E%2E/skills',
    '/c/resources', '/mnt/d/skills', '/cygdrive/e/themes'];
  for (const cwd of [resolve(tmpdir(), 'native workspace'), resolve(tmpdir(), 'another/工作区')]) for (const input of inputs) {
    assert.equal(normalizeResourcePath(input, cwd), resolvePath(input, cwd, { trim: true }), JSON.stringify({ input, cwd }));
  }
  assert.equal(normalizeResourcePath('~', root), homedir());
  assert.equal(normalizeResourcePath('', root), resolve(root), 'Pi blank path resolves to cwd; it is not an empty contribution');
});

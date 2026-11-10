import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { discoverPiSetup } from '../lib/pi-setup.mjs';
import { hasTrustRequiringProjectResources, piProjectTrusted } from '../lib/project-trust.mjs';
import { launch } from './helper.mjs';

function fixture(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'pi-project-trust-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const home = join(root, 'home'), cwd = join(home, 'projects', 'repo');
  mkdirSync(cwd, { recursive: true });
  return { root, home, cwd, hostTrusted: false };
}

test('Pi context trusts a plain workspace without changing host trust', t => {
  const options = fixture(t);
  assert.equal(piProjectTrusted(options), true);
  mkdirSync(join(options.cwd, '.pi'));
  writeFileSync(join(options.cwd, '.pi', 'trust.json'), '{"trusted":true}');
  assert.equal(piProjectTrusted(options), true, 'a Pi trust file is not a resource or an authority');
  assert.equal(options.hostTrusted, false);
});

for (const entry of ['settings.json', 'mcp.json', 'extensions', 'skills', 'prompts', 'themes', 'SYSTEM.md', 'APPEND_SYSTEM.md']) {
  test(`Pi context requires host trust for project .pi/${entry}`, t => {
    const options = fixture(t);
    mkdirSync(join(options.cwd, '.pi'));
    writeFileSync(join(options.cwd, '.pi', entry), '');
    assert.equal(piProjectTrusted(options), false);
    assert.equal(piProjectTrusted({ ...options, hostTrusted: true }), true);
  });
}

test('Pi context gates ancestor .agents/skills but ignores the user-global directory', t => {
  const options = fixture(t);
  mkdirSync(join(options.home, '.agents', 'skills'), { recursive: true });
  assert.equal(piProjectTrusted(options), true);
  mkdirSync(join(options.home, 'projects', '.agents', 'skills'), { recursive: true });
  assert.equal(piProjectTrusted(options), false);
  assert.equal(piProjectTrusted({ ...options, hostTrusted: true }), true);
});

test('Pi context does not mistake ancestor .pi resources for current-project resources', t => {
  const options = fixture(t);
  mkdirSync(join(options.home, 'projects', '.pi'));
  writeFileSync(join(options.home, 'projects', '.pi', 'settings.json'), '{}');
  assert.equal(piProjectTrusted(options), true);
});

test('Pi context handles canonical cwd and fails closed for a missing workspace or dangling resource', t => {
  const options = fixture(t), alias = join(options.root, 'alias');
  symlinkSync(options.cwd, alias);
  assert.equal(piProjectTrusted({ ...options, cwd: alias }), true);
  assert.equal(piProjectTrusted({ ...options, cwd: join(options.root, 'missing') }), false);
  mkdirSync(join(options.cwd, '.pi'));
  symlinkSync(join(options.root, 'missing'), join(options.cwd, '.pi', 'extensions'));
  assert.equal(piProjectTrusted(options), false);
});

test('Pi emits project_trust only for a workspace with protected resources', t => {
  const options = fixture(t);
  assert.equal(hasTrustRequiringProjectResources(options.cwd, options.home), false);
  mkdirSync(join(options.cwd, '.pi'));
  writeFileSync(join(options.cwd, '.pi', 'mcp.json'), '{}');
  assert.equal(hasTrustRequiringProjectResources(options.cwd, options.home), true);
});

test('a project_trust handler cannot admit project resources into an untrusted workspace', t => {
  const options = fixture(t), agent = join(options.root, 'agent');
  mkdirSync(join(options.cwd, '.pi', 'extensions'), { recursive: true });
  writeFileSync(join(options.cwd, '.pi', 'extensions', 'project.js'), 'export default pi => pi.registerCommand("project", { handler() {} });');
  // The bridge refuses trust here natively; the handler's answer never reaches
  // native admission, so the same project resources stay out of the setup.
  assert.equal(piProjectTrusted(options), false);
  assert.deepEqual(discoverPiSetup({ agentDir: agent, cwd: options.cwd, projectTrusted: false }).extensions, []);
  assert.equal(discoverPiSetup({ agentDir: agent, cwd: options.cwd, projectTrusted: true }).extensions.length, 1);
});

// A Pi factory recorded to a file: what the bridge delivers, in order, with the
// answer no bridge can apply. Test data, never part of a reviewed setup.
function trustFactory(t, answer) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'pi-project-trust-event-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const log = join(root, 'events.jsonl'), factory = join(root, 'trust.mjs');
  writeFileSync(factory, `import { appendFileSync } from 'node:fs';
const record = line => appendFileSync(${JSON.stringify(log)}, JSON.stringify(line) + '\\n');
export default function (pi) {
  pi.on('project_trust', (event, ctx) => {
    record({ event: 'project_trust', keys: Object.keys(event), type: event.type, cwd: event.cwd,
      mode: ctx.mode, hasUI: ctx.hasUI, trusted: ctx.isProjectTrusted() });
    return { trusted: ${JSON.stringify(answer)}, remember: true };
  });
  pi.on('session_start', (_event, ctx) => record({ event: 'session_start', trusted: ctx.isProjectTrusted() }));
}
`);
  return { factory, read: () => (existsSync(log) ? readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)) : []) };
}

function protectedWorkspace(t) {
  const workspace = realpathSync(mkdtempSync(join(tmpdir(), 'pi-project-trust-workspace-')));
  t.after(() => rmSync(workspace, { recursive: true, force: true }));
  mkdirSync(join(workspace, '.pi'));
  writeFileSync(join(workspace, '.pi', 'settings.json'), '{}');
  return workspace;
}

test("project_trust fires once with Pi's payload, before session_start, and grants nothing", async t => {
  const workspace = protectedWorkspace(t), recording = trustFactory(t, 'yes');
  const peer = launch(t, [recording.factory], { cwd: workspace });
  await peer.init();
  await peer.start({ project_trusted: false, mode: 'tui' });
  const events = recording.read();
  assert.deepEqual(events.map(entry => entry.event), ['project_trust', 'session_start']);
  assert.deepEqual(events[0].keys, ['type', 'cwd']);
  assert.equal(events[0].type, 'project_trust');
  assert.equal(events[0].cwd, workspace);
  assert.equal(events[0].mode, 'tui');
  assert.equal(events[0].hasUI, true);
  // Subscribing changed no trust: the untrusted snapshot is what the handler sees.
  assert.equal(events[0].trusted, false);
  // The "yes" it returned is a claim about a natively decided state, never a grant.
  assert.equal(events[1].trusted, false);
  // Pi resolves a cwd once per process; a later session_start does not repeat it.
  await peer.start({ project_trusted: false, mode: 'tui' });
  assert.deepEqual(recording.read().map(entry => entry.event), ['project_trust', 'session_start', 'session_start']);
  await peer.waitStderr(/project_trust: .* answered "yes" but octet decided workspace trust natively \(untrusted\); native admission is unchanged/);
  await peer.close();
});

test('project_trust cannot revoke native trust either, and is not emitted for a plain workspace', async t => {
  const workspace = protectedWorkspace(t), recording = trustFactory(t, 'no');
  const peer = launch(t, [recording.factory], { cwd: workspace });
  await peer.init();
  await peer.start({ project_trusted: true, mode: 'tui' });
  const events = recording.read();
  assert.deepEqual(events.map(entry => entry.event), ['project_trust', 'session_start']);
  assert.equal(events[0].trusted, true);
  assert.equal(events[1].trusted, true);
  await peer.waitStderr(/project_trust: .* answered "no" but octet decided workspace trust natively \(trusted\); native admission is unchanged/);
  await peer.close();

  // Pi skips the event when a workspace has no protected resource to decide about.
  const plain = realpathSync(mkdtempSync(join(tmpdir(), 'pi-project-trust-plain-')));
  t.after(() => rmSync(plain, { recursive: true, force: true }));
  const silent = trustFactory(t, 'yes'), quiet = launch(t, [silent.factory], { cwd: plain });
  await quiet.init();
  await quiet.start({ project_trusted: false, mode: 'tui' });
  assert.deepEqual(silent.read().map(entry => entry.event), ['session_start']);
  await quiet.close();
});

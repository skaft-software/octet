// Pi settings projection. A mirrored setting only reaches Octet where a native
// destination exists (session defaults through resources_discover); every other
// user-set key is diagnosed once by name, never invented into a mapping.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { discoverPiSetup } from '../lib/pi-setup.mjs';

function temporary(t) {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-settings-')));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function put(path, source) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, source);
}

// Pi 1.0.2 settings with native Octet equivalents (theme/provider/model/thinking
// level) plus the keys that have none: compaction enable/reserve/keep, image
// auto-resize, steering and queue delivery, and the active-tool selection.
const piSettings = {
  theme: 'pi-projection-theme',
  defaultProvider: 'pi-projection-provider',
  defaultModel: 'pi-projection-model',
  defaultThinkingLevel: 'high',
  compaction: { enabled: false, reserveTokens: 4096 },
  images: { autoResize: false },
  steeringMode: 'all',
  followUpMode: 'all',
  queueMode: 'all',
  defaultTools: ['read', '+bash'],
  defaultProjectTrust: 'ask',
};

function agentWithSettings(root) {
  const agent = join(root, 'home/.pi/agent');
  put(join(agent, 'settings.json'), JSON.stringify(piSettings));
  return agent;
}

test('pi settings: mapped keys project to native session defaults and unmapped keys produce exactly one diagnostic', async t => {
  const root = temporary(t);
  const agent = agentWithSettings(root);
  const setup = discoverPiSetup({ agentDir: agent, cwd: root });

  // Mapped: the three destinations the native contributions accept.
  assert.equal(setup.defaultTheme, 'pi-projection-theme');
  assert.equal(setup.defaultThinkingLevel, 'high');
  assert.deepEqual(setup.defaultModel, { provider: 'pi-projection-provider', model: 'pi-projection-model' });

  // Diagnosed: names only, sorted, one grouping level deep.
  assert.deepEqual(setup.unmappedSettings, ['compaction.enabled', 'compaction.reserveTokens',
    'defaultTools', 'followUpMode', 'images.autoResize', 'queueMode', 'steeringMode']);
  assert.equal(setup.unmappedSettingsCount, 7);
  assert.equal(setup.diagnostics.length, 1, JSON.stringify(setup.diagnostics));
  const [message] = setup.diagnostics;
  assert.match(message, /^settings\.json: 7 Pi settings not applied \(no Octet equivalent\): /);
  for (const name of setup.unmappedSettings) assert.ok(message.includes(name), name);
  // Trust state is never read by the mirror, so it is never named.
  assert.ok(!message.includes('defaultProjectTrust'));
  assert.ok(!setup.unmappedSettings.includes('defaultProjectTrust'));
  // Names only: no setting values reach a diagnostic.
  for (const value of ['false', '4096', '"+bash"', 'read']) assert.ok(!message.includes(value), value);
});

test('pi settings: the unmapped list is bounded and counts the omitted names', async t => {
  const root = temporary(t);
  const agent = join(root, 'home/.pi/agent');
  put(join(agent, 'settings.json'), JSON.stringify(Object.fromEntries(
    Array.from({ length: 24 }, (unused, index) => [`customSetting${String(index).padStart(2, '0')}`, index]))));
  const setup = discoverPiSetup({ agentDir: agent, cwd: root });
  assert.equal(setup.unmappedSettingsCount, 24);
  assert.equal(setup.unmappedSettings.length, 16);
  assert.equal(setup.diagnostics.length, 1);
  assert.match(setup.diagnostics[0], /^settings\.json: 24 Pi settings not applied \(no Octet equivalent\): customSetting00, /);
  assert.ok(setup.diagnostics[0].endsWith(', +8 more'), setup.diagnostics[0]);
});

// The destination itself: the real resources_discover path turns the projected
// defaults into the native contribution and exposes no field for the rest.
test('pi settings: projected defaults reach the resources_discover contribution', async t => {
  let discoverResources;
  try { ({ discoverResources } = await import('../lib/resources.mjs')); }
  catch (error) {
    // The contribution path loads the adapter's Pi runtime, which a workspace
    // without the extension's npm dependencies cannot import.
    if (error?.code !== 'ERR_MODULE_NOT_FOUND') throw error;
    t.skip('adapter runtime dependencies are not installed here');
    return;
  }
  const root = temporary(t);
  const agent = join(root, 'home/.pi/agent');
  // A valid theme file would be a second contribution; only the projected
  // session defaults are under test here.
  put(join(agent, 'settings.json'), JSON.stringify({ ...piSettings, theme: undefined }));
  const diagnostics = [];
  const store = { controller: new AbortController() };
  const runtime = {
    mirrorPiSetup: true, config: { pi_agent_dir: agent }, host: { project_trusted: false },
    events: new Map(), features: new Set(), diagnostics,
    require() {}, bind() {}, assertSessionOwner() {}, diagnostic(message) { diagnostics.push(message); },
    metadata: () => ({ hooks: ['resources_discover'] }),
    queued: (_store, run) => run(), flush: async () => {},
  };
  const result = await discoverResources(runtime, {
    payload: { cwd: root, reason: 'startup' },
    context: { resource_owner: { session_id: 'projected', extension_instance_id: 'projected', process_generation: 0 } },
  }, store);
  const paths = result.resource_paths;
  assert.deepEqual(paths.default_model, { provider: 'pi-projection-provider', model: 'pi-projection-model' });
  assert.equal(paths.default_thinking_level, 'high');
  // No invented destination: the diagnosed keys have no wire field at all.
  for (const field of ['default_tools', 'compaction', 'images', 'steering_mode', 'follow_up_mode', 'queue_mode'])
    assert.equal(paths[field], undefined, field);
  // The grouped diagnostic travels the same path exactly once.
  assert.equal(diagnostics.length, 1, JSON.stringify(diagnostics));
  assert.match(diagnostics[0], /^Pi setup settings\.json: 7 Pi settings not applied/);
});

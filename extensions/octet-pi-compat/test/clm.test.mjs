import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { root } from './helper.mjs';

// Explicitly supplied, unchanged b84a9d7c upstream source only. Registration
// capture is not native/runtime repair evidence for CLM's seven behavioral gates.
const entry = process.env.PI_CLM_PATH;
test('unchanged pinned pi-clm 1.0.0 captures real model-turn registrations, not runtime qualification', {
  skip: !entry && 'set PI_CLM_PATH to reviewed pi-clm b84a9d7c root index.ts',
}, t => {
  const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');
  assert.equal(hash(entry), 'd5e9d73034eb87e28dc9909a2d8ac28b85a1969fd234c7a8f3eddecb71e3fde6');
  assert.equal(hash(join(dirname(entry), 'src/index.ts')), 'a6d1c464bebdcc927a0eb6d0bb1ff319c3e5cdf5bf844e03b599ec91169f7170');
  assert.equal(JSON.parse(readFileSync(join(dirname(entry), 'package.json'), 'utf8')).version, '1.0.0');
  const home = mkdtempSync(join(tmpdir(), 'octet-clm-capture-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const result = spawnSync(process.execPath, [join(root, 'runner.mjs'), '--inspect', entry], {
    encoding: 'utf8', timeout: 10000, maxBuffer: 1048576, cwd: home,
    env: { HOME: home, USERPROFILE: home, TMPDIR: home, PI_OFFLINE: '1' },
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  const frames = result.stdout.trim().split('\n').map(line => JSON.parse(line));
  assert.equal(frames.length, 1, 'exactly one registration capture frame');
  const metadata = frames[0].result;
  assert.deepEqual(metadata.hooks, [
    'after_tool_call', 'before_prompt', 'before_provider_request', 'before_tool_call',
    'model_turn_end', 'model_turn_start', 'provider_context', 'resources_discover',
    'session_before_compact', 'session_compact', 'session_end', 'session_start', 'session_tree',
  ]);
  assert.deepEqual(metadata.events, [
    'before_agent_start', 'before_provider_request', 'context', 'resources_discover',
    'session_before_compact', 'session_compact', 'session_shutdown', 'session_start',
    'session_tree', 'tool_call', 'tool_result', 'turn_end', 'turn_start',
  ]);
  assert.deepEqual(metadata.tools.map(tool => tool.name), ['live_context_annotate', 'live_context_recall']);
  assert.deepEqual(metadata.commands.map(command => command.name), ['clm', 'clm-compact']);
  // No provider/session/UI callback is invoked by --inspect. Native durability,
  // snapshots, projection and security-profile-bound replay remain open work.
});

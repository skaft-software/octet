import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configure } from '../configure.mjs';
import { root } from './helper.mjs';

const parse = file => {
  const done = spawnSync('python3', ['-c', 'import sys,tomllib,json;print(json.dumps(tomllib.load(open(sys.argv[1],"rb"))))', file], { encoding: 'utf8' });
  assert.equal(done.status, 0, done.stderr);
  return JSON.parse(done.stdout);
};

// Pi's registerFlag takes an optional default (examples/extensions/preset.ts registers
// `--preset` with type "string" and none). octet's manifest requires a typed default,
// and the adapter's own registerFlag already reads an absent one as the type's zero value
// (lib/api.mjs registerFlag). Registrations that reach configure without a default
// (captured elsewhere, or reused from a cache) must produce the same valid TOML.
test('configure writes a parseable manifest for flags registered without a default', t => {
  const dir = mkdtempSync(join(tmpdir(), 'octet-flag-default-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const output = join(dir, 'octet-pi-compat');
  const capturedRegistrations = { tools: [], commands: [], hooks: [], shortcuts: [], events: [], tool_renderers: [], flags: [
    { name: 'preset', type: 'string', description: 'Preset configuration to use' },
    { name: 'plan', type: 'boolean' },
    { name: 'limit', type: 'integer', default: 3 },
  ] };
  configure({ output, extensions: [join(root, 'test/fixtures/core.ts')], reviewed: true, capturedRegistrations });
  const toml = readFileSync(join(output, 'extension.toml'), 'utf8');
  assert.doesNotMatch(toml, /undefined/);
  const flags = Object.fromEntries(parse(join(output, 'extension.toml')).contributes.flags.map(flag => [flag.name, flag]));
  assert.deepEqual([flags.preset.default, flags.plan.default, flags.limit.default], ['', false, 3]);
  assert.equal(flags.preset.description, 'Preset configuration to use');
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { piProjectTrusted } from '../lib/project-trust.mjs';

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

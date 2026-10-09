// Synthetic, filesystem-only discovery. Factory text deliberately throws if
// executed; no installed Pi code, package manager, credentials or network.
import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { discoverPiSetup, resolveManagedPackage, themeCandidates } from '../lib/pi-setup.mjs';

const factory = 'throw new Error("DISCOVERY MUST NOT EXECUTE FACTORIES"); export default () => {};';
function put(path, value) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, typeof value === 'string' ? value : JSON.stringify(value));
  return path;
}
function fixture(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'octet-package-discovery-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const agentDir = join(root, 'agent');
  const cwd = join(root, 'project');
  mkdirSync(agentDir); mkdirSync(cwd);
  return { root, agentDir, cwd, project: join(cwd, '.pi'), discover: options => discoverPiSetup({ agentDir, cwd, ...options }) };
}
function installed(base, name, version = '1.2.3', pi) {
  const root = join(base, 'npm/node_modules', name);
  put(join(root, 'package.json'), { name, version, ...(pi === undefined ? {} : { pi }) });
  put(join(root, 'extensions/a.ts'), factory);
  put(join(root, 'extensions/b.ts'), factory);
  put(join(root, 'skills/keep/SKILL.md'), '# keep');
  put(join(root, 'skills/drop/SKILL.md'), '# drop');
  put(join(root, 'prompts/a.md'), 'a');
  put(join(root, 'themes/a.json'), { name: 'a' });
  return root;
}
function settings(base, value) { put(join(base, 'settings.json'), value); }

for (const source of ['npm:plain@1.2.3', 'npm:@owner/scoped@1.2.3', 'npm:plain', 'npm:@owner/scoped', 'npm:plain@v1.2.3+build']) {
  test(`managed npm name-only root: ${source}`, t => {
    const f = fixture(t);
    const name = source.includes('@owner') ? '@owner/scoped' : 'plain';
    const root = installed(f.agentDir, name);
    settings(f.agentDir, { packages: [source] });
    assert.equal(resolveManagedPackage(f.agentDir, source), root);
    const setup = f.discover();
    assert.deepEqual(setup.extensions, [join(root, 'extensions/a.ts'), join(root, 'extensions/b.ts')]);
    assert.deepEqual(setup.diagnostics, []);
  });
}

for (const spec of ['plain@^1.0.0', '@owner/scoped@>=1.0.0 <2.0.0', 'plain@~1.2', 'plain@latest']) {
  test(`ranges/tags fail closed without locked semver: ${spec}`, t => {
    const f = fixture(t);
    const name = spec.startsWith('@') ? '@owner/scoped' : 'plain';
    const root = installed(f.agentDir, name);
    settings(f.agentDir, { packages: [`npm:${spec}`] });
    const setup = f.discover();
    assert.deepEqual(setup.extensions, []);
    assert.deepEqual(setup.skillsPaths, []);
    assert.ok(setup.diagnostics.some(line => line.includes('locked semver dependency')));
    assert.equal(JSON.parse(readFileSync(join(root, 'package.json'))).version, '1.2.3');
  });
}

test('missing, mismatched, invalid-version and traversal npm sources are diagnosed without fallback', t => {
  const f = fixture(t);
  const wrong = installed(f.agentDir, 'wrong', '1.2.4');
  installed(f.agentDir, 'invalid', 'not-a-version');
  settings(f.agentDir, { packages: ['npm:missing@1.2.3', 'npm:wrong@1.2.3', 'npm:invalid', 'npm:../wrong', 'npm:@scope/../../wrong'] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.ok(setup.diagnostics.some(line => line.includes('missing@') && line.includes('never installs')));
  assert.ok(setup.diagnostics.some(line => line.includes('1.2.4 does not match 1.2.3')));
  assert.ok(setup.diagnostics.some(line => line.includes('no valid version')));
  assert.ok(setup.diagnostics.some(line => line.includes('unsupported source syntax')));
  assert.equal(resolveManagedPackage(f.agentDir, 'npm:../wrong'), undefined);
  assert.equal(resolveManagedPackage(f.agentDir, 'wrong'), undefined);
  assert.equal(JSON.parse(readFileSync(join(wrong, 'package.json'))).version, '1.2.4');
});

test('empty arrays disable each package kind before factory selection', t => {
  const f = fixture(t);
  installed(f.agentDir, 'empty');
  settings(f.agentDir, { packages: [{ source: 'npm:empty', extensions: [], skills: [], prompts: [], themes: [] }] });
  const setup = f.discover();
  for (const key of ['extensions', 'skillsPaths', 'promptsPaths', 'themesPaths']) assert.deepEqual(setup[key], []);
  assert.deepEqual(setup.diagnostics, []);
});

test('package includes, excludes and exact +/- overrides use Pi selector precedence', t => {
  const f = fixture(t);
  const root = installed(f.agentDir, 'selectors');
  settings(f.agentDir, { packages: [{
    source: 'npm:selectors',
    extensions: ['extensions/*.ts', '!**/a.ts', '+extensions/a.ts', '-extensions/b.ts'],
    skills: ['*', '!drop', '+skills/drop', '-skills/keep'],
    prompts: [], themes: [],
  }] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, [join(root, 'extensions/a.ts')]);
  assert.deepEqual(setup.skillsPaths, [join(root, 'skills/drop/SKILL.md')]);
  assert.deepEqual(setup.diagnostics, []);
});

test('manifest resources override conventional directories, including manifest-only and empty kinds', t => {
  const f = fixture(t);
  const root = installed(f.agentDir, 'manifest', '1.2.3', { extensions: ['custom/*.ts', '!**/drop.ts'], skills: [], themes: ['palettes'] });
  const kept = put(join(root, 'custom/kept.ts'), factory);
  put(join(root, 'custom/drop.ts'), factory);
  const theme = put(join(root, 'palettes/custom.json'), { name: 'custom' });
  settings(f.agentDir, { packages: ['npm:manifest'] });
  let setup = f.discover();
  assert.deepEqual(setup.extensions, [kept]);
  assert.deepEqual(setup.skillsPaths, []);
  assert.deepEqual(setup.promptsPaths, []); // no undeclared convention fallback for a string source
  assert.deepEqual(setup.themesPaths, [theme]);
  settings(f.agentDir, { packages: [{ source: 'npm:manifest' }] });
  setup = f.discover();
  assert.deepEqual(setup.skillsPaths, []); // explicitly declared empty remains disabled
  assert.deepEqual(setup.promptsPaths, [join(root, 'prompts/a.md')]); // object entry's per-kind default
});

test('autoload:false selects only manifest/default resource deltas, never unfiltered factories', t => {
  const f = fixture(t);
  const root = installed(f.agentDir, 'disabled', '1.2.3', { extensions: ['extensions'], skills: ['custom'] });
  const custom = put(join(root, 'custom/kept/SKILL.md'), '# custom');
  settings(f.agentDir, { packages: [{ source: 'npm:disabled', autoload: false, extensions: ['+extensions/b.ts', '-extensions/b.ts', 'extensions/a.ts'], skills: ['custom/**'] }] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, [join(root, 'extensions/a.ts')]);
  assert.deepEqual(setup.skillsPaths, [custom]);
  assert.deepEqual(setup.promptsPaths, []);
  assert.deepEqual(setup.themesPaths, []);
});

test('unsupported selector syntax suppresses a kind rather than loading defaults', t => {
  const f = fixture(t);
  installed(f.agentDir, 'unsupported');
  const global = put(join(f.agentDir, 'extensions/global.ts'), factory);
  settings(f.agentDir, { packages: [{ source: 'npm:unsupported', extensions: ['extensions/{a,b}.ts'] }], extensions: ['!**/[ab].ts'] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.ok(setup.diagnostics.some(line => line.includes('package npm:unsupported extensions') && line.includes('skipped')));
  assert.ok(setup.diagnostics.some(line => line.includes('settings.extensions') && line.includes('skipped')));
  assert.equal(existsSync(global), true);
});

test('project npm storage and name identity override global versions only when trusted', t => {
  const f = fixture(t);
  const global = installed(f.agentDir, '@owner/overridden', '1.0.0');
  const project = installed(f.project, '@owner/overridden', '2.0.0');
  settings(f.agentDir, { packages: ['npm:@owner/overridden@1.0.0'] });
  settings(f.project, { packages: ['npm:@owner/overridden@2.0.0'] });
  assert.deepEqual(f.discover().extensions, [join(global, 'extensions/a.ts'), join(global, 'extensions/b.ts')]);
  assert.deepEqual(f.discover({ projectTrusted: true }).extensions, [join(project, 'extensions/a.ts'), join(project, 'extensions/b.ts')]);
  settings(f.project, { packages: ['npm:@owner/overridden@3.0.0'] });
  const mismatch = f.discover({ projectTrusted: true });
  assert.deepEqual(mismatch.extensions, []); // no arbitrary global fallback
  assert.ok(mismatch.diagnostics.some(line => line.includes('2.0.0 does not match 3.0.0')));
});

test('trusted project autoload:false deltas operate on the global installation before factories', t => {
  const f = fixture(t);
  const root = installed(f.agentDir, 'delta', '1.2.3');
  settings(f.agentDir, { packages: ['npm:delta@1.2.3'] });
  // Its own version is not the delta base: Pi borrows the user source/storage.
  settings(f.project, { packages: [{ source: 'npm:delta@9.9.9', autoload: false, extensions: ['-extensions/a.ts'], skills: ['!drop'] }] });
  const setup = f.discover({ projectTrusted: true });
  assert.deepEqual(setup.extensions, [join(root, 'extensions/b.ts')]);
  assert.deepEqual(setup.skillsPaths, [join(root, 'skills/keep/SKILL.md')]);
  assert.deepEqual(setup.diagnostics, []);
  settings(f.project, { packages: [{ source: 'npm:delta', autoload: false, extensions: ['!{a,b}.ts'] }] });
  const unsupported = f.discover({ projectTrusted: true });
  assert.deepEqual(unsupported.extensions, []); // cannot bypass a delta via global autoload
  assert.ok(unsupported.diagnostics.some(line => line.includes('resource kind skipped')));
});

test('global/project path bases and project-wins defaults/deep settings merge', t => {
  const f = fixture(t);
  const global = put(join(f.agentDir, 'extra/g.ts'), factory);
  const project = put(join(f.project, 'extra/p.ts'), factory);
  const prompt = put(join(f.project, 'custom/p.md'), 'project');
  settings(f.agentDir, { extensions: ['extra'], defaultProvider: 'global-provider', defaultModel: 'global-model', defaultThinkingLevel: 'high', theme: 'global', compaction: { enabled: true, reserveTokens: 100 }, defaultTools: ['read', 'bash'] });
  settings(f.project, { extensions: ['extra'], prompts: ['custom'], defaultModel: 'project-model', defaultThinkingLevel: 'off', theme: 'project', compaction: { reserveTokens: 200 }, defaultTools: ['-bash'] });
  const setup = f.discover({ projectTrusted: true });
  assert.deepEqual(setup.extensions, [project, global]);
  assert.deepEqual(setup.promptsPaths, [prompt]);
  assert.deepEqual(setup.defaultModel, { provider: 'global-provider', model: 'project-model' });
  assert.equal(setup.defaultThinkingLevel, 'off');
  assert.equal(setup.defaultTheme, 'project');
  assert.deepEqual(setup.settings.compaction, { enabled: true, reserveTokens: 200 });
  assert.deepEqual(setup.settings.defaultTools, ['read', 'bash', '-bash']);
});

test('untrusted project skips all .pi settings, packages and resources, even explicit global references', t => {
  const f = fixture(t);
  const global = put(join(f.agentDir, 'extensions/global.ts'), factory);
  put(join(f.project, 'settings.json'), '{ invalid project settings must not even be parsed');
  put(join(f.project, 'extensions/project.ts'), factory);
  put(join(f.project, 'skills/project/SKILL.md'), '# project');
  put(join(f.project, 'prompts/project.md'), 'project');
  put(join(f.project, 'themes/project.json'), { name: 'project' });
  installed(f.project, 'untrusted');
  settings(f.agentDir, { defaultThinkingLevel: 'high' });
  let setup = f.discover();
  assert.deepEqual(setup.extensions, [global]);
  assert.deepEqual(setup.diagnostics, []);
  assert.equal(setup.defaultThinkingLevel, 'high');
  for (const key of ['skillsPaths', 'promptsPaths', 'themesPaths']) assert.deepEqual(setup[key], []);
  settings(f.agentDir, { extensions: [join(f.project, 'extensions')], packages: [join(f.project, 'npm/node_modules/untrusted')] });
  setup = f.discover();
  assert.deepEqual(setup.extensions, [global]);
  assert.ok(setup.diagnostics.some(line => line.includes('untrusted project .pi')));
});

test('top-level overrides filter automatic resources and use exact skill-directory selectors', t => {
  const f = fixture(t);
  const keep = put(join(f.agentDir, 'extensions/keep.ts'), factory);
  put(join(f.agentDir, 'extensions/drop.ts'), factory);
  const skill = put(join(f.agentDir, 'skills/keep/SKILL.md'), '# keep');
  put(join(f.agentDir, 'skills/drop/SKILL.md'), '# drop');
  settings(f.agentDir, { extensions: ['!*.ts', '+extensions/keep.ts'], skills: ['!drop'] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, [keep]);
  assert.deepEqual(setup.skillsPaths, [skill]);
  assert.deepEqual(setup.diagnostics, []);
});

test('reviewed installed git host/path forms share identity and resolve correct scope without network', t => {
  const f = fixture(t);
  const global = put(join(f.agentDir, 'git/github.com/owner/repo/extensions/global.ts'), factory);
  const project = put(join(f.project, 'git/github.com/owner/repo/extensions/project.ts'), factory);
  settings(f.agentDir, { packages: ['git:github.com/owner/repo.git@main'] });
  settings(f.project, { packages: ['git:git@github.com:owner/repo.git@reviewed'] });
  assert.deepEqual(f.discover().extensions, [global]);
  assert.deepEqual(f.discover({ projectTrusted: true }).extensions, [project]);
  settings(f.project, { packages: ['https://github.com/owner/repo.git@reviewed'] });
  assert.deepEqual(f.discover({ projectTrusted: true }).extensions, [project]);
});

test('local files/directories and nested index/package entrypoints resolve from the scope base', t => {
  const f = fixture(t);
  const direct = put(join(f.agentDir, 'direct.ts'), factory);
  const nested = put(join(f.agentDir, 'tree/nested/index.ts'), factory);
  const declared = put(join(f.agentDir, 'tree/declared/entry.ts'), factory);
  put(join(f.agentDir, 'tree/declared/package.json'), { pi: { extensions: ['entry.ts'] } });
  const project = put(join(f.project, 'standalone/index.ts'), factory);
  settings(f.agentDir, { packages: ['./direct.ts', pathToFileURL(join(f.agentDir, 'tree')).href] });
  settings(f.project, { packages: ['standalone'] });
  const setup = f.discover({ projectTrusted: true });
  // Package order is project first; nested directories honor package entries.
  assert.deepEqual(setup.extensions, [project, direct, declared, nested]);
  assert.deepEqual(setup.diagnostics, []);
});

for (const { label, entry, enabled } of [
  { label: 'string default', entry: source => source, enabled: true },
  { label: 'object default', entry: source => ({ source }), enabled: true },
  { label: 'empty extensions', entry: source => ({ source, extensions: [] }), enabled: false },
  { label: 'nonmatching includes', entry: source => ({ source, extensions: ['other.ts'] }), enabled: false },
  { label: 'matching include', entry: source => ({ source, extensions: ['*.ts'] }), enabled: true },
  { label: 'negative glob', entry: source => ({ source, extensions: ['!**/*.ts'] }), enabled: false },
  { label: 'exact inclusion after exclusion', entry: source => ({ source, extensions: ['!**/*.ts', `+${source}`] }), enabled: true },
  { label: 'exact exclusion wins', entry: source => ({ source, extensions: ['!**/*.ts', `+${source}`, `-${source}`] }), enabled: false },
  { label: 'autoload false without deltas', entry: source => ({ source, autoload: false }), enabled: false },
  { label: 'autoload false with empty deltas', entry: source => ({ source, autoload: false, extensions: [] }), enabled: false },
  { label: 'autoload false with matching include', entry: source => ({ source, autoload: false, extensions: ['*.ts'] }), enabled: true },
  { label: 'autoload false last exclusion wins', entry: source => ({ source, autoload: false, extensions: [`+${source}`, '!**/*.ts'] }), enabled: false },
  { label: 'autoload false last inclusion wins', entry: source => ({ source, autoload: false, extensions: [`-${source}`, '*.ts'] }), enabled: true },
]) {
  test(`singleton local-file package selection: ${label}`, t => {
    const f = fixture(t);
    const source = put(join(f.agentDir, 'direct.ts'), factory);
    settings(f.agentDir, { packages: [entry(source)] });
    const before = snapshot(f.root);
    const setup = f.discover();
    assert.deepEqual(setup.extensions, enabled ? [source] : []);
    for (const key of ['skillsPaths', 'promptsPaths', 'themesPaths']) assert.deepEqual(setup[key], []);
    assert.deepEqual(setup.diagnostics, []);
    assert.deepEqual(snapshot(f.root), before);
  });
}

for (const extensions of [['{direct,other}.ts'], ['[ab].ts'], ['@(direct).ts'], '*.ts']) {
  test(`singleton local-file package refuses unsupported selectors: ${JSON.stringify(extensions)}`, t => {
    const f = fixture(t);
    const source = put(join(f.agentDir, 'direct.ts'), factory);
    settings(f.agentDir, { packages: [{ source, extensions }] });
    const setup = f.discover();
    assert.deepEqual(setup.extensions, []);
    assert.ok(setup.diagnostics.some(line => line.includes(`package ${source} extensions`) && line.includes('resource kind skipped')));
  });
}

for (const { label, selectors, enabled, unsupported } of [
  { label: 'no deltas retain global', selectors: () => undefined, enabled: true },
  { label: 'empty deltas retain global', selectors: () => [], enabled: true },
  { label: 'nonmatching deltas retain global', selectors: () => ['!other.ts'], enabled: true },
  { label: 'negative glob disables global', selectors: () => ['!**/*.ts'], enabled: false },
  { label: 'exact exclusion disables global', selectors: source => [`-${source}`], enabled: false },
  { label: 'ordered inclusion retains global', selectors: source => [`-${source}`, '*.ts'], enabled: true },
  { label: 'unsupported delta suppresses global', selectors: () => ['!{direct,other}.ts'], enabled: false, unsupported: true },
]) {
  test(`trusted project singleton local-file delta: ${label}`, t => {
    const f = fixture(t);
    const source = put(join(f.agentDir, 'direct.ts'), factory);
    settings(f.agentDir, { packages: ['./direct.ts'] });
    settings(f.project, { packages: [{ source: pathToFileURL(source).href, autoload: false, extensions: selectors(source) }] });
    assert.deepEqual(f.discover().extensions, [source]); // no project settings without trust
    const setup = f.discover({ projectTrusted: true });
    assert.deepEqual(setup.extensions, enabled ? [source] : []);
    if (unsupported) assert.ok(setup.diagnostics.some(line => line.includes('extensions') && line.includes('resource kind skipped')));
    else assert.deepEqual(setup.diagnostics, []);
  });
}

test('singleton local-file package restrictions cannot be bypassed by automatic or explicit rediscovery', t => {
  const f = fixture(t);
  const source = put(join(f.agentDir, 'extensions/direct.ts'), factory);
  for (const extensions of [[], ['!**/*.ts'], ['{direct,other}.ts']]) {
    settings(f.agentDir, { packages: [{ source, extensions }], extensions: [source] });
    assert.deepEqual(f.discover().extensions, []);
  }
});

test('trusted project singleton delta can enable a globally disabled local file', t => {
  const f = fixture(t);
  const source = put(join(f.agentDir, 'direct.ts'), factory);
  settings(f.agentDir, { packages: [{ source: './direct.ts', extensions: [] }] });
  settings(f.project, { packages: [{ source, autoload: false, extensions: [`+${source}`] }] });
  assert.deepEqual(f.discover().extensions, []);
  const setup = f.discover({ projectTrusted: true });
  assert.deepEqual(setup.extensions, [source]);
  assert.deepEqual(setup.diagnostics, []);
});

function snapshot(dir) {
  return readdirSync(dir).sort().flatMap(name => {
    const path = join(dir, name), stat = lstatSync(path);
    if (stat.isDirectory()) return snapshot(path);
    if (stat.isSymbolicLink()) return [];
    return [[path, stat.mtimeMs, readFileSync(path).toString('base64')]];
  });
}

test('linked parents/leaves, missing and oversized resources are diagnosed and never returned or modified', t => {
  const f = fixture(t);
  const outside = join(f.root, 'outside');
  const linkedFile = put(join(outside, 'entry.ts'), factory);
  symlinkSync(outside, join(f.agentDir, 'linked'));
  mkdirSync(join(f.agentDir, 'extensions'));
  symlinkSync(linkedFile, join(f.agentDir, 'extensions/link.ts'));
  put(join(f.agentDir, 'extensions/huge.ts'), ' '.repeat(1024 * 1024 + 1));
  put(join(f.agentDir, 'themes/huge.json'), ' '.repeat(256 * 1024 + 1));
  settings(f.agentDir, { extensions: ['linked/entry.ts', 'missing.ts'], packages: ['linked'] });
  const before = snapshot(f.root);
  const setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.deepEqual(setup.themesPaths, []);
  assert.ok(setup.diagnostics.some(line => line.includes('missing.ts') && line.includes('missing')));
  assert.ok(setup.diagnostics.some(line => line.includes('linked/entry.ts') && line.includes('symlinks')));
  assert.ok(setup.diagnostics.some(line => line.includes('link.ts') && line.includes('symlinks')));
  assert.ok(setup.diagnostics.some(line => line.includes('huge.ts') && line.includes('mirror limit')));
  assert.deepEqual(themeCandidates([join(f.agentDir, 'themes/huge.json')]), []);
  assert.deepEqual(snapshot(f.root), before);
});

test('invalid or oversized settings never fall back to unfiltered automatic factories', t => {
  const f = fixture(t);
  put(join(f.agentDir, 'extensions/global.ts'), factory);
  put(join(f.project, 'extensions/project.ts'), factory);
  put(join(f.agentDir, 'settings.json'), '{ invalid settings');
  settings(f.project, {});
  let setup = f.discover({ projectTrusted: true });
  assert.deepEqual(setup.extensions, [join(f.project, 'extensions/project.ts')]);
  assert.ok(setup.diagnostics.some(line => line.includes('settings.json')));
  put(join(f.agentDir, 'settings.json'), `{"extensions":["${'x'.repeat(1024 * 1024)}"]}`);
  setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.ok(setup.diagnostics.some(line => line.includes('mirror limit')));
});

test('missing or malformed manifest declarations do not enable conventional factories', t => {
  const f = fixture(t);
  installed(f.agentDir, 'missing-entry', '1.2.3', { extensions: ['missing.ts'] });
  installed(f.agentDir, 'bad-selector', '1.2.3', { extensions: ['extensions/{a,b}.ts'] });
  installed(f.agentDir, 'bad-kind', '1.2.3', { extensions: 'extensions' });
  settings(f.agentDir, { packages: ['npm:missing-entry', 'npm:bad-selector', 'npm:bad-kind'] });
  const setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.ok(setup.diagnostics.some(line => line.includes('missing.ts') && line.includes('missing')));
  assert.ok(setup.diagnostics.some(line => line.includes('pi.extensions') && line.includes('resource kind skipped')));
});

test('unsupported ignore rules fail closed and explicit entrypoints remain available', t => {
  const f = fixture(t);
  const entry = put(join(f.agentDir, 'extensions/a.ts'), factory);
  put(join(f.agentDir, 'extensions/.gitignore'), 'a.ts\n');
  settings(f.agentDir, {});
  let setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.ok(setup.diagnostics.some(line => line.includes('.gitignore rules cannot be honored')));
  // Pi exact file entries are not directory autoload; they bypass ignore rules.
  settings(f.agentDir, { extensions: [entry] });
  setup = f.discover();
  assert.deepEqual(setup.extensions, [entry]);
});

test('directory and factory bounds diagnose truncation instead of silently dropping entries', t => {
  const f = fixture(t);
  for (let index = 0; index < 257; index++) put(join(f.agentDir, `extensions/e${String(index).padStart(3, '0')}.ts`), factory);
  const setup = f.discover();
  assert.equal(setup.extensions.length, 64);
  assert.ok(setup.diagnostics.some(line => line.includes('256-entry mirror budget')));
  assert.ok(setup.diagnostics.some(line => line.includes('only the first 64')));
});

test('a refused linked index or ignore file cannot trigger alternate unfiltered factories', t => {
  const f = fixture(t);
  const outside = put(join(f.root, 'outside.ts'), factory);
  mkdirSync(join(f.agentDir, 'extensions'), { recursive: true });
  symlinkSync(outside, join(f.agentDir, 'extensions/index.ts'));
  put(join(f.agentDir, 'extensions/alternate.ts'), factory);
  let setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.ok(setup.diagnostics.some(line => line.includes('index.ts') && line.includes('symlinks')));
  rmSync(join(f.agentDir, 'extensions/index.ts'));
  symlinkSync(outside, join(f.agentDir, 'extensions/.gitignore'));
  setup = f.discover();
  assert.deepEqual(setup.extensions, []);
  assert.ok(setup.diagnostics.some(line => line.includes('rules cannot be honored')));
});

test('read-only discovery preserves source bytes/mtimes and never executes a package entrypoint', t => {
  const f = fixture(t);
  installed(f.agentDir, 'readonly');
  settings(f.agentDir, { packages: ['npm:readonly@1.2.3'] });
  const before = snapshot(f.root);
  const setup = f.discover();
  assert.equal(setup.extensions.length, 2);
  assert.deepEqual(setup.diagnostics, []);
  assert.deepEqual(snapshot(f.root), before);
});

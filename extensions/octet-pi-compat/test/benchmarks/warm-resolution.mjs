// Synthetic prepare + inspect, NOT a PTY/full-UI or real-package benchmark.
// Copies adapters into equal-length paths, primes each, then alternates order.
// Executes only the generated factories; never reads the user's Pi setup.
import assert from 'node:assert/strict';
import { cpSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join, resolve, basename } from 'node:path';
import { createHash } from 'node:crypto';
import { cpus, platform, release, arch } from 'node:os';

const [baseline, candidate, destination, count = '9'] = process.argv.slice(2);
if (!baseline || !candidate || !destination) throw Error('Usage: node warm-resolution.mjs BASELINE_ADAPTER CANDIDATE_ADAPTER NEW_OUTPUT_DIR [REPS=9]');
const reps = Number(count);
assert.ok(Number.isInteger(reps) && reps >= 1 && reps <= 100);
const output = resolve(destination);
mkdirSync(output); // refuse to overwrite evidence
const home = join(output, 'home'), agent = join(home, '.pi/agent'), tmp = join(output, 'tmp'), workspace = join(output, 'workspace');
mkdirSync(agent, { recursive: true }); mkdirSync(tmp); mkdirSync(workspace);
const extensions = [];
for (let f = 0; f < 23; f++) {
  const dir = join(home, `factory-${f}`); mkdirSync(dir);
  for (let i = 0; i < 24; i++) writeFileSync(join(dir, `part-${i}.ts`), `export const value: number = ${i};\n`);
  const entry = join(dir, 'index.ts'); extensions.push(entry);
  writeFileSync(entry, Array.from({ length: 24 }, (_, i) => `import {value as v${i}} from './part-${i}';`).join('\n')
    + `\nexport default pi => pi.registerCommand('factory-${f}', {description:String(${Array.from({ length: 24 }, (_, i) => `v${i}`).join('+')}),handler(){}});\n`);
}
writeFileSync(join(agent, 'settings.json'), JSON.stringify({ extensions }));
const variants = { baseline: 'before', candidate: 'after_' };
for (const [variant, source] of [['baseline', baseline], ['candidate', candidate]]) cpSync(resolve(source), join(output, variants[variant]), {
  recursive: true, filter: path => !['.cache', 'test'].includes(basename(path)),
});
const digest = path => createHash('sha256').update(readFileSync(path)).digest('hex');
writeFileSync(join(output, 'environment.json'), JSON.stringify({ node: process.version, executable: process.execPath,
  platform: platform(), release: release(), arch: arch(), cpu: cpus()[0]?.model, reps,
  baseline, candidate, resolutionModule: Object.fromEntries(Object.entries(variants).map(([v, dir]) => [v, digest(join(output, dir, 'lib/jiti-resolve-cache.mjs'))])),
  fixture: { factories: 23, helpersPerFactory: 24, language: 'TypeScript', packages: 'synthetic; no npm corpus' },
}, null, 2));
let expected;
function run(variant, rep) {
  const adapter = join(output, variants[variant]);
  const dir = join(output, `${variant}-${rep}`); mkdirSync(dir);
  const bridge = join(dir, 'octet-pi-compat');
  const env = { PATH: process.env.PATH, HOME: home, TMPDIR: tmp, OCTET_PI_AGENT_DIR: agent,
    OCTET_PI_COMPAT_CACHE: join(output, `${variant}-cache`), OCTET_STARTUP_TRACE: '1' };
  function execute(args) {
    const start = performance.now();
    const p = spawnSync(process.execPath, args, { env, cwd: workspace, encoding: 'utf8', timeout: 120000, maxBuffer: 4194304 });
    assert.equal(p.status, 0, p.error?.message ?? p.stderr);
    return { ms: performance.now() - start, stdout: p.stdout, stderr: p.stderr };
  }
  const prepare = execute([join(adapter, 'auto-configure.mjs'), bridge, 'false']);
  if (rep !== 'prime') assert.match(prepare.stderr, /Reused the captured catalog/);
  const inspect = execute([join(adapter, 'runner.mjs'), '--config', join(bridge, 'bridge.json'), '--inspect']);
  assert.match(inspect.stderr, /factories=23 failures=0/);
  const metadata = JSON.parse(inspect.stdout).result;
  assert.equal(metadata.commands.length, 23);
  expected ??= metadata; assert.deepEqual(metadata, expected);
  const row = { variant, rep, prepare_ms: prepare.ms, inspect_ms: inspect.ms, total_ms: prepare.ms + inspect.ms };
  writeFileSync(join(dir, 'result.json'), JSON.stringify({ ...row, prepare, inspect }, null, 2));
  console.log(JSON.stringify(row));
  return row;
}
for (const variant of Object.keys(variants)) run(variant, 'prime');
// Let source timestamps leave SourceFingerprints' existing racy window, then
// prime both fingerprints once. No source bytes change in the measured legs.
await new Promise(resolve => setTimeout(resolve, 3100));
for (const variant of Object.keys(variants)) run(variant, 'settled');
const rows = [];
for (let rep = 0; rep < reps; rep++) for (const variant of rep % 2 ? ['candidate', 'baseline'] : ['baseline', 'candidate']) rows.push(run(variant, rep));
const median = values => { const a = [...values].sort((a, b) => a - b); return (a[Math.floor((a.length - 1) / 2)] + a[Math.ceil((a.length - 1) / 2)]) / 2; };
const summary = Object.fromEntries(Object.keys(variants).map(variant => [variant,
  Object.fromEntries(['prepare_ms', 'inspect_ms', 'total_ms'].map(metric => [metric, median(rows.filter(row => row.variant === variant).map(row => row[metric]))])),
]));
writeFileSync(join(output, 'summary.json'), JSON.stringify(summary, null, 2));
console.log(JSON.stringify({ medians: summary }));

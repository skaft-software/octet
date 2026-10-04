// Test-only, offline oracle. No adapter implementation is used to generate expectations.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { stripTypeScriptTypes } from 'node:module';

export async function themeColorVectors(repo) {
  const commit = '581e7ba78141a4d8b61cc9d11b8b22ae7e59195e';
  const hashes = {
    'packages/tui/src/oklab.ts': '45b067e6e3605b385f595adecd7c0216f1c6b6686680d5c73f661286de736be6',
    'packages/tui/src/colors.ts': 'd4fe729c424d2c07bc64cf0c3edfdbf5642865cba395dfb37234c6c88d65f468',
  };
  const source = path => {
    const text = execFileSync('git', ['-C', repo, 'show', `${commit}:${path}`], { encoding: 'utf8', timeout: 10000 });
    assert.equal(createHash('sha256').update(text).digest('hex'), hashes[path], path);
    return stripTypeScriptTypes(text);
  };
  const url = text => `data:text/javascript;base64,${Buffer.from(text).toString('base64')}`;
  const math = url(source('packages/tui/src/oklab.ts'));
  const oracle = await import(url(source('packages/tui/src/colors.ts').replace('"./oklab.ts"', JSON.stringify(math))));
  const inputs = [
    'oklch(62% 0.1 200)', 'oklch(100% 0.3 150)', 'okhsl(250 60% 55%)',
    'OKLCH( .62 +.1 -160DEG )', 'OKHSL(-90DEG 1 .5)',
    'oklch(6.2E-1 1e-1 +2E2deg)', 'okhsl(2.5e2deg 6e1% 5.5e1%)',
    'oklch(-0 -0 -0)', 'okhsl(-0 -0 -0)', 'oklch(0.5 1e308 1e308)',
    'okhsl(1e308 1 0.5)', 'oklch(1e-300 0.1 30)',
    'oklch(\u00a0.62\u2009.1\u3000200\ufeff)', 'okhsl(\ufeff250\u202f60%\u200355%)',
  ];
  for (const h of [-720, -90, 0, 29, 120, 210, 264, 359.99]) {
    for (const l of [0, 0.00001, 0.1, 0.5, 0.9, 0.99999, 1]) {
      for (const c of [0, 0.0000001, 0.1, 0.4, 4]) inputs.push(`oklch(${l} ${c} ${h})`);
      for (const s of [0, 0.0000001, 0.25, 0.799999, 0.8, 0.800001, 0.99, 1]) inputs.push(`okhsl(${h} ${s} ${l})`);
    }
  }
  const invalid = [
    'okhsl(30 1 1e-300)', 'oklch(NaN 0.1 0)', 'okhsl(Infinity 1 .5)', 'oklch(1e309 0 0)', 'okhsl(1e309 1 .5)',
    'oklch(50% 1e309 0)', 'okhsl(0 1e309 .5)', 'oklch(-.01 0 0)', 'oklch(101% 0 0)',
    'oklch(.5 -.01 0)', 'okhsl(0 -1% .5)', 'okhsl(0 101% .5)', 'okhsl(0 .5 -1%)',
    'okhsl(0 .5 101%)', 'oklch(.5 10% 0)', 'oklch(.5, .1, 30)', 'okhsl(0, 1, .5)',
    'oklch(50% .1 30rad)', 'okhsl(0rad 1 .5)', 'oklch(.5 .1)', 'okhsl(0 1)',
    'oklch(.5 .1 30 / 1)', 'okhsl(0 1 .5 / 1)', 'oklch(.5 .1 30)junk',
    'oklch(５0% .1 30)', 'okhsl(0 ١ .5)', 'oklch(50 % .1 30)', 'okhsl(0 50 % .5)',
  ];
  for (const input of invalid) assert.throws(() => oracle.parseColor(input), input);
  return { commit, hashes, valid: inputs.map(input => [input, oracle.colorToHex(oracle.parseColor(input))]), invalid };
}

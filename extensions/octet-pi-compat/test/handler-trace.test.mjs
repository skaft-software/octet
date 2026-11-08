import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';

const budgetModule = new URL('../lib/handler-budget.mjs', import.meta.url).href;
const script = `
  import { HookBudget } from ${JSON.stringify(budgetModule)};
  const runtime = { config: { extensions: ['/private/fixture/slow-package/start.ts'] } };
  const store = { controller: new AbortController(), pending: new Set(), state: {} };
  const budget = new HookBudget(runtime, store, 'session_start');
  let finish;
  const handler = new Promise(resolve => { finish = resolve; });
  const run = budget.run({ factory: 0 }, {}, () => handler);
  await new Promise(resolve => setImmediate(resolve));
  process.stderr.write('handler-still-pending\\n');
  finish();
  await run;
`;

for (const enabled of [false, true]) test(`session_start trace attributes an unsettled handler only when enabled (${enabled})`, () => {
  const result = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { OCTET_STARTUP_TRACE: enabled ? '1' : '0' },
    encoding: 'utf8', timeout: 5000,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, '');
  const lines = result.stderr.trim().split('\n');
  if (!enabled) assert.deepEqual(lines, ['handler-still-pending']);
  else {
    assert.equal(lines.length, 3);
    assert.match(lines[0], /^octet-startup: pi-compat\.handler elapsed=\d+us event=session_start factory=slow-package\/start\.ts took=0ms outcome=started$/);
    assert.equal(lines[1], 'handler-still-pending', 'attribution precedes handler settlement');
    assert.match(lines[2], /event=session_start factory=slow-package\/start\.ts took=\d+ms outcome=ok$/);
    assert.equal(result.stderr.includes('/private/fixture'), false);
  }
});

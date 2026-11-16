// Reduced session oracle crash probe. No providers or user inventory are loaded.
// Pi contract: packages/coding-agent/src/core/session-manager.ts:1272-1298
// (appendCompaction), :1590-1595 (branch), :1026-1054 (setSessionFile).
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

function operations(count) {
  const result = [];
  for (let i = 0; i < count; i++) {
    result.push({ op: 'message', as: `u${i}`, message: { role: 'user', content: `${i}:` + 'x'.repeat(1024), timestamp: 0 } });
    result.push({ op: 'message', as: `a${i}`, message: { role: 'assistant', content: [{ type: 'text', text: `answer:${i}` }], api: 'openai-completions', provider: 'fixture', model: 'offline', stopReason: 'stop', timestamp: 0 } });
  }
  return result;
}

test('long oracle reports completed snapshots, memory and actual child exit', { timeout: 120000 }, async t => {
  const directory = mkdtempSync(join(tmpdir(), 'pi-endurance-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const ops = operations(600);
  ops.push({ op: 'compaction', summary: 'reduced summary', firstKept: 'u590' }, { op: 'reopen' },
    { op: 'branch', target: 'a598' }, { op: 'custom', customType: 'private', data: { durable: true } },
    { op: 'reopen' }, { op: 'new', slot: 'original' }, ...operations(1).map(op => ({ ...op, as: undefined })),
    { op: 'switch', slot: 'original' }, { op: 'reopen' });
  const checkpoints = [1199, 1200, 1201, 1203, 1204, 1207, 1208, 1209];
  const child = spawn(process.execPath, [new URL('./session-driver.mjs', import.meta.url).pathname, '1.5.0'], { stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); });
  let stdout = '', stderr = '', overflow = false;
  child.stdout.setEncoding('utf8').on('data', data => {
    if (stdout.length + data.length >= 32 * 1024 * 1024) { overflow = true; child.kill('SIGKILL'); }
    else stdout += data;
  });
  child.stderr.setEncoding('utf8').on('data', data => { stderr = (stderr + data).slice(-65536); });
  child.stdin.end(JSON.stringify({ directory, cases: [{ name: 'long', operations: ops, checkpoints }], diagnostics: true }));
  const status = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', (code, signal) => resolve({ code, signal })); });
  t.diagnostic(JSON.stringify({ host: { pid: process.pid, rss: process.memoryUsage().rss }, child: { pid: child.pid, ...status }, stderr_tail: stderr }));
  assert.equal(overflow, false, 'oracle output exceeds the reduced fixture bound');
  assert.deepEqual(status, { code: 0, signal: null }, stderr);
  const events = stderr.trim().split('\n').map(line => JSON.parse(line));
  assert.equal(events.length, checkpoints.length * 2);
  for (const index of checkpoints) {
    const pair = events.filter(event => event.index === index);
    assert.deepEqual(pair.map(event => event.stage), ['snapshot_begin', 'snapshot_end']);
    assert.ok(pair.every(event => event.pid === child.pid && event.rss > 0 && event.heap_used > 0 && event.max_rss > 0));
  }
  const steps = JSON.parse(stdout).results[0].steps;
  assert.equal(steps.length, checkpoints.length);
  assert.equal(steps[0].state.entries.length, 1200);
  assert.equal(steps[5].state.entries.length, 2, 'new session has an independent tree');
  assert.deepEqual(steps[6].state, steps[4].state, 'switch restores the original branch and durable private state');
  assert.equal(steps.at(-1).state.entries.length, 1202);
  assert.equal(steps.at(-1).state.path.length, 1199);
  assert.deepEqual(steps.at(-1).state, steps.at(-1).disk.state);
});

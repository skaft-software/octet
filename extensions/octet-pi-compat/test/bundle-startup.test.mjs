import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { delimiter, dirname, join } from 'node:path';
import { root } from './helper.mjs';
import { workerExecArgv } from '../lib/transport.mjs';

// The reviewed bundle is launched by the manifest, not by the file form the
// other tests use: `node --eval "import($OCTET_EXTENSION_DIR/runner.mjs)..."`.
// The transport worker inherits that parent execArgv, so the regression has to
// start the real child process and speak JSON-RPC over the piped stdio the host
// provides.
function manifestEntrypoint() {
  const manifest = readFileSync(join(root, 'extension.toml'), 'utf8');
  const section = manifest.split(/^\[entrypoint\]\s*$/m)[1]?.split(/^\[/m)[0] ?? '';
  const command = /^\s*command\s*=\s*("(?:[^"\\]|\\.)*")\s*$/m.exec(section)?.[1];
  const args = /^\s*args\s*=\s*(\[[^\n]*\])\s*$/m.exec(section)?.[1];
  assert.ok(command && args, '[entrypoint] must keep a readable command/args declaration');
  return { command: JSON.parse(command), args: JSON.parse(args) };
}

// The module-input form this bundle shipped before the fix. Node refuses
// `--input-type` for the worker's file entry, so the worker launch has to drop
// the inherited string-input selectors for any parent that still uses it.
const originalEvalEntrypoint = () => ({
  command: process.execPath,
  args: ['--input-type=module', '--eval', `await import(${JSON.stringify(join(root, 'runner.mjs'))})`],
});

const initialize = id => ({ jsonrpc: '2.0', id, method: 'initialize', params: {
  api_version: '0.4',
  protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], limits: { max_concurrent_requests: 8 } },
} });
const unknownCommand = id => ({ jsonrpc: '2.0', id, method: 'command/execute', params: { name: 'not-configured', arguments: [] } });
const shutdown = id => ({ jsonrpc: '2.0', id, method: 'shutdown', params: {} });

function startBundle(t, entrypoint = manifestEntrypoint()) {
  const { command, args } = entrypoint;
  const child = spawn(command, args, {
    cwd: root, stdio: ['pipe', 'pipe', 'pipe'],
    // The host clears its environment and provides the extension directory;
    // the test binary's directory stands in for the host's PATH lookup.
    env: { ...process.env, OCTET_EXTENSION_DIR: root, NODE_OPTIONS: '',
      PATH: `${dirname(process.execPath)}${delimiter}${process.env.PATH ?? ''}` },
  });
  let stdout = '', stderr = '', next = 1;
  const pending = new Map();
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-65536); });
  child.stdin.on('error', () => {});
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    stdout += chunk;
    let end;
    while ((end = stdout.indexOf('\n')) >= 0) {
      const line = stdout.slice(0, end); stdout = stdout.slice(end + 1);
      let frame;
      try { frame = JSON.parse(line); } catch { assert.fail(`bundle stdout is not JSON-RPC: ${line.slice(0, 200)}`); }
      const waiter = pending.get(frame.id);
      if (waiter) { pending.delete(frame.id); waiter.resolve(frame); }
    }
  });
  const closed = new Promise(resolve => child.on('close', (code, signal) => {
    for (const waiter of pending.values()) waiter.reject(new Error(`bundle exited (${code}) before replying: ${stderr}`));
    pending.clear(); resolve({ code, signal });
  }));
  t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill(); });
  function request(frame) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(frame.id); reject(new Error(`no reply for ${frame.method}: ${stderr}`)); }, 15000);
      pending.set(frame.id, { resolve: value => { clearTimeout(timer); resolve(value); },
        reject: error => { clearTimeout(timer); reject(error); } });
      child.stdin.write(JSON.stringify(frame) + '\n');
    });
  }
  return { child, request, closed, stderr: () => stderr, next: () => next++ };
}

test('the manifest entrypoint starts its transport worker and serves the host over piped stdin', async t => {
  const bundle = startBundle(t);
  assert.equal((await bundle.request(initialize(bundle.next()))).result.api_version, '0.4');
  assert.equal((await bundle.request(unknownCommand(bundle.next()))).error.code, -32602);
  assert.deepEqual((await bundle.request(shutdown(bundle.next()))).result, {});
  assert.deepEqual(await bundle.closed, { code: 0, signal: null });
  assert.doesNotMatch(bundle.stderr(), /ERR_INPUT_TYPE_NOT_ALLOWED|\[pi-compat transport\]/);
});

test('the module-input eval form still starts the worker through the inherited-argv filter', async t => {
  const bundle = startBundle(t, originalEvalEntrypoint());
  assert.equal((await bundle.request(initialize(bundle.next()))).result.api_version, '0.4');
  assert.deepEqual((await bundle.request(shutdown(bundle.next()))).result, {});
  assert.deepEqual(await bundle.closed, { code: 0, signal: null });
  assert.doesNotMatch(bundle.stderr(), /ERR_INPUT_TYPE_NOT_ALLOWED|\[pi-compat transport\]/);
});

test('a piped stdin waits for the host without ending the worker', async t => {
  const bundle = startBundle(t);
  await new Promise(resolve => setTimeout(resolve, 750));
  assert.equal(bundle.child.exitCode, null, bundle.stderr());
  assert.equal(bundle.stderr(), '');
  assert.equal((await bundle.request(initialize(bundle.next()))).result.api_version, '0.4');
  await bundle.request(shutdown(bundle.next()));
  assert.equal((await bundle.closed).code, 0);
});

test('worker exec argv drops only the inherited string-input selectors', () => {
  assert.deepEqual(workerExecArgv(['--input-type=module', '--eval', 'await import(x)', '--max-old-space-size=2048', '--enable-source-maps']),
    ['--max-old-space-size=2048', '--enable-source-maps']);
  assert.deepEqual(workerExecArgv(['--max-old-space-size=2048', '--input-type', 'module', '--eval', 'x']), ['--max-old-space-size=2048']);
  assert.deepEqual(workerExecArgv(['-e', 'x', '-p', 'y', '--print=z', '--eval=w']), []);
  assert.deepEqual(workerExecArgv(['--eval', "import(x).catch(() => process.exit(1))"]), []);
  assert.deepEqual(workerExecArgv(['--inspect-brk']), ['--inspect-brk']);
});

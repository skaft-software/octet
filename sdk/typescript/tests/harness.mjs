import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export const cli = fileURLToPath(new URL('../process/cli.mjs', import.meta.url));
export const fixture = fileURLToPath(new URL('fixtures/process-author.mjs', import.meta.url));
export const context = {workspace: '/local/workspace', execution_scope: null, host: {}, resource_owner: {
  session_id: 'host-owner', extension_instance_id: 'host-instance', process_generation: 2,
}};
export const initialize = (overrides = {}) => ({api_version: '0.4', octet_version: '0.8.2',
  extension: {name: 'test-extension', version: '0.1.0', manifest_path: '/local/extension.toml', source: 'explicit'},
  workspace: '/local/workspace', capabilities: {filesystem: 'none', process: false, network: false}, host: {},
  contributes: {tools: ['test_tool'], commands: ['test-command'], hooks: [], context: false, ui: [],
    tool_renderers: [], notifications: false, confirmations: false, presentation: false, menu: false, flags: [], shortcuts: []},
  protocol: {version: '0.4', required_features: ['request_cancellation', 'content_parts'],
    optional_features: ['request_progress', 'dynamic_tools', 'runtime_commands', 'future_optional'], limits: {max_concurrent_requests: 4}},
  ...overrides});
export const request = (id, method, params = {}) => ({jsonrpc: '2.0', id, method, params});
export const tool = (id, arguments_ = {}, overrides = {}) => request(id, 'tool/call', {name: 'test_tool', arguments: arguments_, context, ...overrides});

export function harness(t, {source = fixture, cliPath = cli, launcher, env = {}} = {}) {
  const child = spawn(launcher ?? process.execPath, launcher ? [] : [cliPath, 'run', source], {env: {...process.env, OCTET_EXTENSION_API_VERSION: '0.4', ...env}, stdio: ['pipe', 'pipe', 'pipe']});
  t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); });
  child.stdin.on('error', () => {});
  let output = '', stderr = '';
  const frames = [], waiters = [];
  child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
  child.stderr.on('data', data => { stderr += data; });
  child.stdout.on('data', data => {
    output += data;
    while (output.includes('\n')) {
      const end = output.indexOf('\n');
      const line = output.slice(0, end); output = output.slice(end + 1);
      assert(Buffer.byteLength(line) < 1_048_576);
      const frame = JSON.parse(line); assert.equal(frame.jsonrpc, '2.0');
      frames.push(frame);
      for (const waiter of [...waiters]) if (waiter.predicate(frame)) {
        waiters.splice(waiters.indexOf(waiter), 1); clearTimeout(waiter.timer); waiter.resolve(frame);
      }
    }
  });
  const exited = new Promise(resolve => child.once('close', (code, signal) => resolve({code, signal})));
  const wait = (predicate, ms = 5000) => {
    const known = frames.find(predicate); if (known) return Promise.resolve(known);
    return new Promise((resolve, reject) => {
      const waiter = {predicate, resolve};
      waiter.timer = setTimeout(() => { waiters.splice(waiters.indexOf(waiter), 1); reject(new Error(`Timed out waiting for frame; stderr=${stderr}`)); }, ms);
      waiters.push(waiter);
    });
  };
  t.after(() => { for (const waiter of waiters) clearTimeout(waiter.timer); });
  return {child, frames, exited, wait,
    send: message => child.stdin.write(JSON.stringify(message) + '\n'),
    reply: id => wait(frame => frame.id === id),
    progress: id => wait(frame => frame.method === '$/progress' && frame.params.request_id === id),
    stderr: () => stderr,
    async ready(params = initialize()) {
      this.send(request(1, 'initialize', params));
      const result = await this.reply(1); assert(result.result, JSON.stringify(result)); return result.result;
    },
    async stop(id = 999) {
      this.send(request(id, 'shutdown')); assert.deepEqual((await this.reply(id)).result, {});
      assert.equal((await exited).code, 0); assert.equal(output, '');
    },
  };
}

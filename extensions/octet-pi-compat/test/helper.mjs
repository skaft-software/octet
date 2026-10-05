import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
export const root = fileURLToPath(new URL('../', import.meta.url));
export const owner = { session_id: 'host-issued-owner', extension_instance_id: 'host-issued-instance', process_generation: 2 };
export const host = { session_id: 'actual-session', session_name: 'Synthetic', model: 'test-model', model_view: { id: 'test-model', name: 'Test Model', api: 'openai-responses', provider: 'test', reasoning: false, input: ['text'], context_window: 32768, max_tokens: 1024, cost: { input: 1000000, output: 2000000, cache_read: 100000, cache_write: 200000 } }, reasoning: null, active_skills: [] };
export function inspect(extensions) {
  const result = spawnSync(process.execPath, [join(root, 'runner.mjs'), '--inspect', ...extensions], { encoding: 'utf8', timeout: 10000, maxBuffer: 1048576 });
  assert.equal(result.status, 0, result.stderr); return JSON.parse(result.stdout).result;
}
export function launch(t, extensions = [join(root, 'test/fixtures/core.ts')], options = {}) {
  const metadata = inspect(extensions);
  const child = spawn(process.execPath, [options.runner || join(root, 'runner.mjs'), ...(options.config ? ['--config', options.config] : extensions)], { cwd: options.cwd || root, stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, ...options.env } });
  let stderr = '', next = 1;
  const queue = [], waiters = [], seen = [], editors = new Map();
  child.stderr.on('data', b => { stderr = (stderr + b).slice(-65536); });
  child.stdin.on('error', () => {});
  createInterface({ input: child.stdout }).on('line', line => {
    let frame;
    try { frame = JSON.parse(line); } catch { assert.fail(`protocol stdout corrupted: ${line.slice(0, 200)}`); }
    seen.push(frame); if (seen.length > 128) seen.shift();
    if (options.auto !== false && frame.id !== undefined && frame.method && !options.hold?.includes(frame.method)) {
      let result;
      if (frame.method === 'ui/open') {
        result = { columns: options.columns || 80, rows: options.rows || 24 };
        if (frame.params.placement === 'editor' && options.editorFence !== false) {
          result.editor_mount_id = `host-editor-${frame.params.surface_id}`;
          editors.set(frame.params.surface_id, { mount_id: result.editor_mount_id, input_revision: 0 });
        }
      }
      else if (frame.method === 'ui/autocomplete/register') result = { accepted: options.completionAccepted ?? true };
      else if (frame.method === 'composer/get') result = { text: options.composer ?? 'existing draft' };
      else if (frame.method === 'session/append_entry') result = { entry_id: 'host-entry' };
      else if (frame.method === 'composer/set' && frame.params.editor_checkpoint) {
        const { input_revision, checkpoint_revision } = frame.params.editor_checkpoint;
        result = { input_revision, checkpoint_revision };
      }
      else if (['ui/close', 'composer/set', 'composer/insert', 'session/set_name', 'shortcut/register', 'session/set_label', 'session/send_user_message', 'session/send_message', 'tools/set_active'].includes(frame.method)) result = {};
      if (result !== undefined) send({ jsonrpc: '2.0', id: frame.id, result });
    }
    const at = waiters.findIndex(w => w.match(frame));
    if (at >= 0) { const [w] = waiters.splice(at, 1); clearTimeout(w.timer); w.resolve(frame); }
    else {
      if (frame.method === 'ui/frame') { const old = queue.findIndex(f => f.method === 'ui/frame' && f.params.surface_id === frame.params.surface_id); if (old >= 0) queue.splice(old, 1); }
      queue.push(frame); if (queue.length > 128) queue.shift();
    }
  });
  t.after(() => { child.kill(); for (const w of waiters.splice(0)) { clearTimeout(w.timer); w.reject(new Error('test ended')); } });
  function send(frame) { child.stdin.write(JSON.stringify(frame) + '\n'); }
  function wait(match, timeout = 7000) {
    const i = queue.findIndex(match); if (i >= 0) return Promise.resolve(queue.splice(i, 1)[0]);
    return new Promise((resolve, reject) => {
      const w = { match, resolve, reject, timer: setTimeout(() => { waiters.splice(waiters.indexOf(w), 1); reject(new Error(`RPC timeout: ${stderr}`)); }, timeout) }; waiters.push(w);
    });
  }
  function request(method, params) { const id = next++; send({ jsonrpc: '2.0', id, method, params }); return { id, response: wait(f => f.id === id && !f.method) }; }
  function context(extra = {}) { return { workspace: options.cwd || root, resource_owner: owner, host: { ...host, ...extra } }; }
  return { child, send, wait, request, context, seen, metadata, stderr: () => stderr,
    async init(features = ['remote_ui', 'request_progress', 'composer', 'editor_handoff', 'session_entries', 'lifecycle_events', 'lifecycle_events_v2', 'shortcuts', 'message_injection', 'active_tools', 'input_transform_v1']) {
      const result = await request('initialize', { api_version: '0.4', workspace: options.cwd || root, host,
        contributes: { tools: metadata.tools.map(t => t.name), commands: metadata.commands.map(c => c.name), hooks: metadata.hooks, tool_renderers: metadata.tool_renderers },
        flag_values: [{ name: 'test-option', value: 'host-value' }],
        protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: features, limits: { max_concurrent_requests: 8 } },
      }).response;
      assert.ok(result.result, JSON.stringify(result)); return result.result;
    },
    async start(extra = {}) { const result = await request('hook/run', { hook: 'session_start', payload: { binding: owner }, context: context(extra) }).response; assert.ok(result.result, JSON.stringify(result)); return result; },
    command(name, args = [], extra = {}) { return request('command/execute', { name, arguments: args, context: context(extra) }); },
    call(mode, extra = {}) { return request('tool/call', { name: 'core', arguments: { mode }, context: context(extra) }); },
    editorKey(surface_id, key, kind = 'press', modifiers = []) {
      const editor = editors.get(surface_id); assert.ok(editor, 'synthetic editor mount is open');
      send({ jsonrpc: '2.0', method: 'ui/key', params: { surface_id, key, kind, modifiers,
        editor_input: { mount_id: editor.mount_id, input_revision: ++editor.input_revision } } });
    },
    notify(method, params) { send({ jsonrpc: '2.0', method, params }); },
    async close() { const exited = once(child, 'exit'); assert.deepEqual((await request('shutdown', {}).response).result, {}); assert.equal((await exited)[0], 0); },
  };
}

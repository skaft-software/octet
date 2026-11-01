import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createAPI } from '../lib/api.mjs';

const loadingError = 'Extension runtime not initialized. Action methods cannot be called during extension loading.';
test('Pi action stubs fail before feature checks and argument validation during loading', async t => {
  const runtime = new Runtime({ extensions: [] }, {});
  t.after(() => runtime.uninstallChildren());
  const api = createAPI(runtime, 0);
  for (const method of ['sendMessage', 'sendUserMessage', 'appendEntry', 'setSessionName', 'getSessionName', 'setLabel',
    'getActiveTools', 'getAllTools', 'getSettings', 'setActiveTools', 'getCommands', 'getThinkingLevel', 'setThinkingLevel']) {
    assert.throws(() => api[method](), { name: 'Error', message: loadingError }, method);
  }
  await assert.rejects(api.setModel(), { name: 'Error', message: 'Extension runtime not initialized' });
  api.registerCommand('loaded', { handler() {} });
  runtime.loaded = true;
  assert.throws(() => api.getActiveTools(), { message: loadingError });
  runtime.actionsInitialized = true;
  assert.throws(() => api.getActiveTools(), /unsupported_feature active_tools/);
  runtime.forgetFactory(0, '/failed');
  assert.throws(() => api.getActiveTools(), /failed extension factory/);
});

test('Pi commands ignore unknown options while validating known fields', t => {
  const runtime = new Runtime({ extensions: [] }, {});
  t.after(() => runtime.uninstallChildren());
  const api = createAPI(runtime, 0), handler = () => {};
  api.registerCommand('config', { description: 'configure', prompt: 'unused', futureOption: true, usage: { ignored: true }, handler });
  assert.equal(runtime.commands.get('config').definition.handler, handler);
  assert.deepEqual(runtime.metadata().commands, [{ name: 'config', description: 'configure' }]);
  assert.throws(() => api.registerCommand('bad', { handler: false, prompt: true }), /command handler/);
  assert.throws(() => api.registerCommand('bad', { handler, getArgumentCompletions: false }), /getArgumentCompletions/);
});

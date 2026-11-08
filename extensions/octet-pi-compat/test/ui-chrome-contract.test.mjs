import test from 'node:test';
import assert from 'node:assert/strict';
import { chromeAPI, dialogAPI } from '../lib/ui-api.mjs';

test('plain confirm and input retain authoritative native host requests', async () => {
  const calls = [];
  const api = dialogAPI(() => { throw new Error('unexpected remote surface'); }, {
    confirm: async (...args) => { calls.push(['confirm', ...args]); return true; },
    input: async (...args) => { calls.push(['input', ...args]); return 'native-value'; },
  });
  assert.equal(await api.confirm('Title', 'Message'), true);
  assert.equal(await api.confirm('Title', 'Message', {}), true);
  assert.equal(await api.input('Input', 'accepted-placeholder'), 'native-value');
  assert.deepEqual(calls, [['confirm', 'Title', 'Message'], ['confirm', 'Title', 'Message'], ['input', 'Input']]);
  const controller = new AbortController(); controller.abort();
  assert.equal(await api.confirm('Aborted', 'Question', {signal: controller.signal}), false);
  assert.equal(calls.length, 3);
});

test('chrome setters update local state and coalesce delivery without synchronous host requests', async () => {
  const controller = new AbortController(), owner = {session_id: 's', extension_instance_id: 'i', process_generation: 1};
  const store = {controller, id: 7, live: true, state: {owner, host: {tools_expanded: false}, alive: true}};
  const calls = [];
  const runtime = {
    features: new Set(['remote_ui']),
    require: feature => assert.equal(feature, 'remote_ui'), assertOwner: current => assert.equal(current, store),
    transport: {
      requestSync() { assert.fail('cosmetic state must not synchronously request the host'); },
      request(method, params, options) {
        assert.equal(method, 'ui/chrome'); assert.equal(params.resource_owner, owner);
        assert.equal(params.parent_request_id, 7); assert.equal(options.parent, undefined);
        calls.push(params.chrome);
        return Promise.resolve({tools_expanded: store.state.host.tools_expanded});
      },
    },
    backgroundError(error) { assert.fail(error.message); },
  };
  const api = chromeAPI(runtime, store);
  assert.equal(api.getToolsExpanded(), false);
  store.state.host.tools_expanded = true; // Models the native Ctrl+O context snapshot.
  assert.equal(api.getToolsExpanded(), true);
  api.setToolsExpanded(false); assert.equal(api.getToolsExpanded(), false);
  api.setTitle('Title'); api.setWorkingMessage('Working'); api.setWorkingVisible(false);
  api.setWorkingIndicator({frames: ['\x1b[31mS\x1b[0m'], intervalMs: 17}); api.setHiddenThinkingLabel('Hidden');
  api.setWorkingMessage(); api.setWorkingIndicator(); api.setHiddenThinkingLabel();
  assert.equal(calls.length, 0, 'setters return before any host round trip');
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(calls.length, 6, 'each kind is delivered once, with its latest value');
  assert.deepEqual(calls.filter(call => ['working_message', 'working_indicator', 'hidden_thinking'].includes(call.kind)), [
    {kind: 'working_message', message: null}, {kind: 'working_indicator', frames: null, interval_ms: null},
    {kind: 'hidden_thinking', label: null},
  ]);
  const before = calls.length;
  assert.throws(() => api.setWorkingIndicator({frames: ['\x1b]2;unsafe\x07']}));
  assert.throws(() => api.setWorkingVisible('false'));
  assert.throws(() => api.setWorkingIndicator({intervalMs: -1}));
  assert.equal(calls.length, before);
});

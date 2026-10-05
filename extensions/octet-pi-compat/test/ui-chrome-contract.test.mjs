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

test('chrome getter consumes authoritative receipt rather than cached JS state', () => {
  const controller = new AbortController(), owner = {session_id: 's'};
  const store = {controller, id: 7, live: true, state: {owner}};
  let nativeExpanded = false;
  const calls = [];
  const runtime = {
    require: feature => assert.equal(feature, 'remote_ui'), assertOwner: current => assert.equal(current, store),
    transport: {requestSync(method, params, options) {
      assert.equal(method, 'ui/chrome'); assert.equal(params.resource_owner, owner);
      assert.equal(options.parent, 7); assert.equal(options.signal, controller.signal);
      calls.push(params.chrome);
      if (params.chrome.kind === 'tools_expanded') nativeExpanded = params.chrome.expanded;
      return {tools_expanded: nativeExpanded};
    }},
  };
  const api = chromeAPI(runtime, store);
  assert.equal(api.getToolsExpanded(), false);
  nativeExpanded = true; // Models native Ctrl+O, not an adapter setter.
  assert.equal(api.getToolsExpanded(), true);
  api.setToolsExpanded(false); assert.equal(api.getToolsExpanded(), false);
  api.setTitle('Title'); api.setWorkingMessage('Working'); api.setWorkingVisible(false);
  api.setWorkingIndicator({frames: ['\x1b[31mS\x1b[0m'], intervalMs: 17}); api.setHiddenThinkingLabel('Hidden');
  api.setWorkingMessage(); api.setWorkingIndicator(); api.setHiddenThinkingLabel();
  assert.deepEqual(calls.slice(-3), [
    {kind: 'working_message', message: null}, {kind: 'working_indicator', frames: null, interval_ms: null},
    {kind: 'hidden_thinking', label: null},
  ]);
  const before = calls.length;
  assert.throws(() => api.setWorkingIndicator({frames: ['\x1b]2;unsafe\x07']}));
  assert.throws(() => api.setWorkingVisible('false'));
  assert.throws(() => api.setWorkingIndicator({intervalMs: -1}));
  assert.equal(calls.length, before);
});

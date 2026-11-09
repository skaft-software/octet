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

test('chrome getter consumes authoritative receipt rather than cached JS state', async () => {
  const controller = new AbortController(), owner = {session_id: 's'};
  const store = {controller, id: 7, live: true, state: {owner, alive: true, host: {}}};
  let nativeExpanded = false;
  const calls = [];
  const receipt = (method, params) => {
    assert.equal(method, 'ui/chrome'); assert.equal(params.resource_owner, owner);
    calls.push(params.chrome);
    if (params.chrome.kind === 'tools_expanded') nativeExpanded = params.chrome.expanded;
    return {tools_expanded: nativeExpanded};
  };
  const runtime = {
    features: new Set(['remote_ui']),
    require: feature => assert.equal(feature, 'remote_ui'), assertOwner: current => assert.equal(current, store),
    backgroundError: error => { throw error; },
    transport: {
      requestSync(method, params, options) {
        assert.equal(options.parent, 7); assert.equal(options.signal, controller.signal);
        return receipt(method, params);
      },
      request(method, params) { return Promise.resolve(receipt(method, params)); },
    },
  };
  const api = chromeAPI(runtime, store);
  assert.equal(api.getToolsExpanded(), false);
  nativeExpanded = true; // Models native Ctrl+O, not an adapter setter.
  assert.equal(api.getToolsExpanded(), true);
  api.setToolsExpanded(false); assert.equal(api.getToolsExpanded(), false);
  api.setTitle('Title'); api.setWorkingMessage('Working'); api.setWorkingVisible(false);
  api.setWorkingIndicator({frames: ['\x1b[31mS\x1b[0m'], intervalMs: 17}); api.setHiddenThinkingLabel('Hidden');
  api.setWorkingMessage(); api.setWorkingIndicator(); api.setHiddenThinkingLabel();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(calls.filter(call => ['working_message', 'working_indicator', 'hidden_thinking'].includes(call.kind)), [
    {kind: 'working_message', message: null}, {kind: 'working_indicator', frames: null, interval_ms: null},
    {kind: 'hidden_thinking', label: null},
  ]);
  assert.equal(api.getToolsExpanded(), false);
  nativeExpanded = true;
  assert.equal(api.getToolsExpanded(), true, 'native changes after setter admission remain authoritative');
  const before = calls.length;
  assert.throws(() => api.setWorkingIndicator({frames: ['\x1b]2;unsafe\x07']}));
  assert.throws(() => api.setWorkingVisible('false'));
  assert.throws(() => api.setWorkingIndicator({intervalMs: -1}));
  assert.equal(calls.length, before);
});

test('a disclosure read failure preserves the last native observation', () => {
  const controller = new AbortController(), owner = {session_id: 's'};
  const store = {controller, id: 7, live: true, state: {owner, alive: true, host: {}}};
  let fail = false;
  const errors = [];
  const runtime = {
    features: new Set(['remote_ui']), require() {}, assertOwner() {},
    backgroundError: error => errors.push(error.message),
    transport: {requestSync() { if (fail) throw new Error('read timed out'); return {tools_expanded: true}; }},
  };
  const api = chromeAPI(runtime, store);
  assert.equal(api.getToolsExpanded(), true);
  fail = true;
  assert.equal(api.getToolsExpanded(), true);
  assert.equal(api.getToolsExpanded(), true);
  assert.deepEqual(errors, ['read timed out'], 'a failed read reports once');
});

test('an older disclosure receipt cannot replace a newer pending setter', async () => {
  const controller = new AbortController(), owner = {session_id: 's'};
  const store = {controller, id: 7, live: true, state: {owner, alive: true, host: {}}};
  const requests = [];
  let nativeExpanded = false, reads = 0;
  const runtime = {
    features: new Set(['remote_ui']), require() {}, assertOwner() {},
    backgroundError: error => { throw error; },
    transport: {
      request(_method, params) {
        return new Promise(resolve => requests.push({expanded: params.chrome.expanded, resolve}));
      },
      requestSync() { reads++; return {tools_expanded: nativeExpanded}; },
    },
  };
  const api = chromeAPI(runtime, store);
  api.setToolsExpanded(true);
  await new Promise(resolve => setImmediate(resolve));
  api.setToolsExpanded(false);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(requests.map(request => request.expanded), [true, false]);
  nativeExpanded = true; requests[0].resolve({tools_expanded: true});
  await Promise.resolve();
  assert.equal(api.getToolsExpanded(), false);
  assert.equal(reads, 0, 'the later setter remains pending');
  nativeExpanded = false; requests[1].resolve({tools_expanded: false});
  await Promise.resolve();
  nativeExpanded = true;
  assert.equal(api.getToolsExpanded(), true);
  assert.equal(reads, 1);
});

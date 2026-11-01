import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { beforeAgentStart } from '../lib/before-agent-start.mjs';
import { mcpAPI, startMcpSession } from '../lib/mcp.mjs';
import { setActiveTools } from '../lib/tools.mjs';
import { Runtime } from '../lib/runtime.mjs';

function promptFixture(handlers, host = {}) {
  const store = { state: { workspace: '/workspace', host }, controller: new AbortController(), pending: new Set(), errors: [] };
  const errors = [];
  const runtime = { config: { extensions: ['/one.mjs', '/two.mjs'] }, events: new Map([['before_agent_start', handlers.map((handler, factory) => ({ factory, handler }))]]),
    features: new Set(), tools: new Map(), scope: new AsyncLocalStorage(), require() {}, assertSessionOwner() {}, flush: async () => {}, reportCallbackError: (...args) => errors.push(args) };
  return { runtime, store, errors, run: () => beforeAgentStart(runtime, { prompt: 'question', system_prompt: 'native base' }, store) };
}

test('before_agent_start shares normalized options and lazily renders section mutations in event and context', async () => {
  let options;
  const f = promptFixture([
    (event, ctx) => {
      for (const key of ['images', 'prompt', 'systemPrompt', 'systemPromptOptions', 'type']) assert.ok(Object.hasOwn(event, key));
      options = event.systemPromptOptions;
      assert.deepEqual(options.skills, [{ name: 'api-engineer', description: 'APIs', filePath: '/skills/api-engineer/SKILL.md', baseDir: '/skills/api-engineer', sourceInfo: { source: 'local' } }]);
      assert.deepEqual(options.sections, {});
      options.sections.postman = 'mandate';
      assert.equal(event.systemPrompt, 'native base\n\n<postman>\nmandate\n</postman>');
      assert.equal(ctx.getSystemPrompt(), event.systemPrompt);
    },
    event => {
      assert.equal(event.systemPromptOptions, options);
      event.systemPromptOptions.sections.extra = 'second';
    },
  ], { system_prompt_options: { skills: [{ name: 'api-engineer', description: 'APIs', filePath: '/skills/api-engineer/SKILL.md', baseDir: '/skills/api-engineer', sourceInfo: { source: 'local' } }] } });
  assert.equal((await f.run()).systemPrompt, 'native base\n\n<postman>\nmandate\n</postman>\n\n<extra>\nsecond\n</extra>');
  assert.deepEqual(f.errors, []);
});

test('forced prompt dominates options until cleared, including empty strings', async () => {
  const f = promptFixture([
    event => { event.systemPromptOptions.sections.first = 'one'; return { systemPrompt: '' }; },
    event => { assert.equal(event.systemPrompt, ''); event.systemPromptOptions.forceSystemPrompt = undefined; },
  ]);
  assert.equal((await f.run()).systemPrompt, 'native base\n\n<first>\none\n</first>');
  assert.deepEqual(f.errors, []);
});

test('mutable prompt collections render tool hints, deduplicated guidelines, project files and visible skills', async () => {
  const f = promptFixture([event => {
    const options = event.systemPromptOptions;
    options.toolSnippets.read = 'Read a file';
    options.toolGuidelines.read = [' same ', 'same'];
    options.promptGuidelines.push('same', 'extra');
    options.contextFiles.push({ path: '/workspace/AGENTS.md', content: 'project rule' });
    options.skills.push({ name: 'visible<&', description: 'description', filePath: '/skills/visible/SKILL.md' },
      { name: 'hidden', description: 'hidden', filePath: '/skills/hidden/SKILL.md', disableModelInvocation: true });
    options.cwd = 'C:\\work';
    const text = event.systemPrompt;
    assert.match(text, /<tools>\n- read: Read a file/);
    assert.equal(text.match(/- same/g)?.length, 1);
    assert.match(text, /- extra/);
    assert.match(text, /<project_instructions path="\/workspace\/AGENTS.md">\nproject rule/);
    assert.match(text, /<name>visible&lt;&amp;<\/name>/);
    assert.doesNotMatch(text, /<name>hidden<\/name>/);
    assert.match(text, /<cwd>\nC:\/work\n<\/cwd>/);
  }]);
  await f.run();
  assert.deepEqual(f.errors, []);
});

test('section overrides replace an existing native section rather than duplicating it', async () => {
  const f = promptFixture([event => { event.systemPromptOptions.sections.rules = 'replacement'; }]);
  const result = await beforeAgentStart(f.runtime, { prompt: 'question', system_prompt: 'native base\n\n<rules>\noriginal\n</rules>' }, f.store);
  assert.equal(result.systemPrompt, 'native base\n\n<rules>\nreplacement\n</rules>');
});

for (const explicit of [false, true]) test(`final prompt uses acknowledged tool selection; explicit options edit=${explicit}`, async () => {
  let active = ['read', 'bash'];
  let options;
  const f = promptFixture([event => {
    options = event.systemPromptOptions;
    setActiveTools(f.runtime, 0, ['read']);
    if (explicit) options.selectedTools = ['bash', 'missing', 'hidden'];
  }]);
  f.runtime.features.add('active_tools');
  f.runtime.current = factory => {
    const store = f.runtime.scope.getStore();
    assert.equal(store.factory, factory);
    return store;
  };
  const tools = [{ name: 'read', promptSnippet: 'Read a file' }, { name: 'bash', promptSnippet: 'Run a command' },
    { name: 'hidden', promptSnippet: 'Not declared', exposure: 'hidden' }];
  f.runtime.transport = { requestSync(method, params) {
    if (method === 'tools/snapshot') return { active_tools: [...active], all_tools: tools };
    assert.equal(method, 'tools/set_active');
    active = params.names;
    return {};
  } };
  const result = await f.run();
  assert.deepEqual(f.errors, []);
  assert.deepEqual(active, explicit ? ['bash'] : ['read']);
  assert.deepEqual(options.selectedTools, active);
  assert.match(result.systemPrompt, explicit ? /- bash: Run a command/ : /- read: Read a file/);
  assert.doesNotMatch(result.systemPrompt, explicit ? /- read: Read a file/ : /- bash: Run a command/);
  assert.doesNotMatch(result.systemPrompt, /Not declared|missing/);
});

test('session_start exposes Pi reason and previous file without native owner routing fields', async () => {
  const f = promptFixture([]), events = [];
  const binding = { session_id: 'native-session', extension_instance_id: 'native-instance', process_generation: 2 };
  f.runtime.initialized = true;
  f.runtime.metadata = () => ({ hooks: ['session_start'] });
  f.runtime.bind = params => { assert.equal(params.payload.binding, binding); };
  f.runtime.queued = (_store, work) => work();
  f.runtime.runEvent = async (_type, event) => { events.push(JSON.parse(JSON.stringify(event))); return { action: 'continue' }; };
  for (const payload of [{ binding }, { binding, reason: 'reload', previousSessionFile: '/previous.jsonl' }]) {
    await Runtime.prototype.dispatch.call(f.runtime, { method: 'hook/run', params: { hook: 'session_start', payload } }, f.store);
  }
  assert.deepEqual(events, [
    { type: 'session_start', reason: 'startup' },
    { type: 'session_start', reason: 'reload', previousSessionFile: '/previous.jsonl' },
  ]);
});

test('session_shutdown projects actual Pi reasons and target file while retaining internal retirement', async () => {
  const f = promptFixture([]), events = [];
  const binding = { session_id: 'native-session', extension_instance_id: 'native-instance', process_generation: 2 };
  let retired = 0;
  f.runtime.initialized = true;
  f.runtime.metadata = () => ({ hooks: ['session_end'] });
  f.runtime.bind = params => { assert.equal(params.payload.binding, binding); };
  f.runtime.queued = (_store, work) => work();
  f.runtime.runEvent = async (type, event) => {
    if (type === 'session_shutdown') events.push(JSON.parse(JSON.stringify(event)));
    return { action: 'continue' };
  };
  f.runtime.retire = async state => { assert.equal(state, f.store.state); retired++; };
  for (const reason of ['reload', 'quit', 'new', 'resume', 'fork', 'native-only']) {
    const payload = { binding, reason, duration_ms: 12, outcome: 'interrupted', targetSessionFile: '/actual-target.jsonl' };
    await Runtime.prototype.dispatch.call(f.runtime, { method: 'hook/run', params: { hook: 'session_end', payload } }, f.store);
    assert.equal(payload.binding, binding);
  }
  await Runtime.prototype.dispatch.call(f.runtime, { method: 'hook/run', params: { hook: 'session_end', payload: { binding } } }, f.store);
  assert.equal(retired, 7);
  assert.deepEqual(events, [
    ...['reload', 'quit', 'new', 'resume', 'fork', 'quit'].map(reason => ({
      type: 'session_shutdown', reason, targetSessionFile: '/actual-target.jsonl',
    })),
    { type: 'session_shutdown', reason: 'quit' },
  ]);
});

test('MCP transport and feature failure is a background error, not session_start failure', async () => {
  const errors = [];
  const runtime = { config: { extensions: ['/one.mjs'] }, loaded: false, events: new Map(), assertOwner() {},
    require() { throw Object.assign(new Error('no MCP feature'), { code: -32601 }); }, backgroundError: error => errors.push(error),
    hostCall: async () => { throw new Error('offline SECRET'); } };
  const api = mcpAPI(runtime, 0);
  api.registerMcpServer('http', { url: 'https://example.invalid/mcp' });
  const store = { state: {}, controller: new AbortController() };
  await startMcpSession(runtime, store);
  runtime.require = () => {};
  await startMcpSession(runtime, store);
  assert.equal(errors.length, 2);
  assert.equal(errors.some(error => error.message.includes('SECRET')), false);
  assert.equal(api.getMcpServers().length, 1);
});

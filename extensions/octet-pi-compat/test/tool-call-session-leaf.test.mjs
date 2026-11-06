import assert from 'node:assert/strict';
import test from 'node:test';
import { Runtime } from '../lib/runtime.mjs';
import { owner } from './helper.mjs';

// The host's per-operation SessionDriver binds one leaf before `before_tool_call`
// and, since the grant-forwarding fix, presents exactly that grant on the
// `tool/call` that follows. The adapter must accept it there (and only on
// hook/run and tool/call), so the tool body appends under that grant.
const head = 'head-1';
const grant = { grant_id: 'b'.repeat(64), activation_epoch: 1, operation_id: 'tool-driver:1', owner, expected_head: head };
const params = () => ({ context: { resource_owner: owner, host: { session_leaf_id: head } }, session_leaf: structuredClone(grant) });

function fixture(method, hook) {
  const runtime = new Runtime({ extensions: [] }, {});
  runtime.uninstallChildren();
  const store = { id: 9, live: true, method, hook, controller: new AbortController(), pending: new Set(), errors: [] };
  runtime.active.set(9, store);
  return { runtime, store };
}

test('a tool/call carrying the driver grant binds it for the tool body', () => {
  const { runtime, store } = fixture('tool/call');
  runtime.bind(params(), store);
  assert.deepEqual(store.leaf?.grant, grant);
});

test('a hook/run grant is still accepted', () => {
  const { runtime, store } = fixture('hook/run', 'before_tool_call');
  runtime.bind(params(), store);
  assert.deepEqual(store.leaf?.grant, grant);
});

test('a grant on any other method stays refused', () => {
  for (const method of ['command/execute', 'shortcut/execute', 'tool/render']) {
    const { runtime, store } = fixture(method);
    assert.throws(() => runtime.bind(params(), store), /session_leaf requires hook\/run or tool\/call/, method);
    assert.equal(store.leaf, undefined);
  }
});

test('a tool/call grant for another owner is refused', () => {
  const { runtime, store } = fixture('tool/call');
  const foreign = params();
  foreign.session_leaf.owner = { ...owner, process_generation: owner.process_generation + 1 };
  assert.throws(() => runtime.bind(foreign, store), /not_foreground_owner|session_leaf/);
  assert.equal(store.leaf, undefined);
});

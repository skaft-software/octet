import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createContext } from '../lib/api.mjs';
import { owner } from './helper.mjs';

function fixture() {
  const runtime = Object.create(Runtime.prototype);
  const state = { owner, alive: true, workspace: '/native', host: {} };
  const store = { id: 1, state, controller: new AbortController() };
  return { state, store, context: createContext(runtime, store) };
}

test('pending-message queries preserve actual native false and true snapshots across retained reads', () => {
  const { state, context } = fixture();
  const pending = context.hasPendingMessages;
  state.host = { has_pending_messages: false };
  assert.equal(pending(), false);
  state.host = { has_pending_messages: true };
  assert.equal(pending(), true);
  state.host = { has_pending_messages: false };
  assert.equal(pending(), false);
});

test('missing native pending-message facts refuse instead of inventing an empty queue', () => {
  const { state, context } = fixture();
  assert.throws(() => context.hasPendingMessages(), /unsupported_feature ctx.hasPendingMessages: has_pending_messages snapshot not supplied by the host/);
  state.host.has_pending_messages = false;
  assert.equal(context.hasPendingMessages(), false);
  delete state.host.has_pending_messages;
  assert.throws(() => context.hasPendingMessages(), /snapshot not supplied by the host/);
});

test('retired contexts cannot read pending-message facts belonging to the former owner', () => {
  const { state, context } = fixture();
  state.host.has_pending_messages = true;
  assert.equal(context.hasPendingMessages(), true);
  state.alive = false;
  assert.throws(() => context.hasPendingMessages(), /retained context is unavailable/);
});

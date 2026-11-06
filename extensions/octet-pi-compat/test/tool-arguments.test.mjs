// Pi 1.5 validates tool arguments with pi-schema: coerce first, then check, and
// fail a call that carries `argumentsError` before any schema work. The compat
// path must accept every call Pi accepts (never surfacing an error for valid
// arguments) and fail invalid ones with Pi's messages.
import test from 'node:test';
import assert from 'node:assert/strict';
import { validateToolArguments, validateToolCall } from '../shims/pi-1.5.0/ai/validation.mjs';
import { builtinArguments } from '../lib/builtin-arguments.mjs';

const readTool = { name: 'read', parameters: { type: 'object', properties: { path: { type: 'string' }, offset: { type: 'number' }, limit: { type: 'number' } }, required: ['path'], additionalProperties: false } };
const editTool = {
  name: 'edit',
  parameters: {
    type: 'object',
    properties: {
      path: { type: 'string' },
      edits: { type: 'array', items: { type: 'object', properties: { oldText: { type: 'string' }, newText: { type: 'string' } }, required: ['oldText', 'newText'], additionalProperties: false } },
    },
    required: ['path', 'edits'],
  },
};

test('a valid call never surfaces an error, and coercion matches Pi 1.5', () => {
  assert.deepEqual(validateToolArguments(readTool, { name: 'read', arguments: { path: 'a.txt' } }), { path: 'a.txt' });
  // pi-schema coerce: "42" -> 42, null -> 0 for a number, 42 -> "42" for a string.
  assert.deepEqual(validateToolArguments(readTool, { name: 'read', arguments: { path: 42, limit: '30' } }), { path: '42', limit: 30 });
  assert.deepEqual(validateToolArguments(editTool, { name: 'edit', arguments: { path: 'a', edits: [{ oldText: 'x', newText: 'y' }] } }),
    { path: 'a', edits: [{ oldText: 'x', newText: 'y' }] });
  assert.equal(validateToolCall([readTool], { name: 'read', arguments: { path: 'a' } }).path, 'a');
});

test('a call carrying argumentsError fails the way Pi 1.5 fails it', () => {
  assert.throws(() => validateToolArguments(readTool, { name: 'read', arguments: {}, argumentsError: 'Unexpected token } in JSON\n\nReceived arguments:\n{"path":}' }),
    /^Error: Invalid arguments for tool "read": Unexpected token \} in JSON/);
});

test('an invalid call reports pi-schema paths and messages, not TypeBox keywords', () => {
  assert.throws(() => validateToolArguments(readTool, { name: 'read', arguments: {} }), /- path: missing/);
  assert.throws(() => validateToolArguments(readTool, { name: 'read', arguments: { path: 'a', limit: 'text' } }), /- limit: expected number, got string/);
  assert.throws(() => validateToolArguments(editTool, { name: 'edit', arguments: { path: 'a', edits: [{ oldText: 'x' }] } }), /- edits\.0\.newText: missing/);
  assert.throws(() => validateToolArguments(readTool, { name: 'read', arguments: { path: 'a', extra: 1 } }), /- extra: unknown field/);
  assert.throws(() => validateToolCall([readTool], { name: 'missing', arguments: {} }), /Tool "missing" not found/);
  // The failure still carries the received arguments, as Pi 1.5's message does.
  assert.throws(() => validateToolArguments(readTool, { name: 'read', arguments: {} }), /Received arguments:\n\{\}/);
});

test('builtin arguments are coerced before translation, so valid calls translate', () => {
  assert.deepEqual(builtinArguments('read', { path: 'a.txt' }, undefined, '/w'), { path: '/w/a.txt', limit: 2000 });
  assert.deepEqual(builtinArguments('read', { path: 42, limit: '30', offset: '2' }, undefined, '/w'), { path: '/w/42', limit: 30, offset: 2 });
  assert.deepEqual(builtinArguments('bash', { command: 'ls', timeout: '30' }, undefined, '/w'), { command: 'ls', cwd: '/w', timeout_ms: 30000 });
  assert.deepEqual(builtinArguments('bash', { command: 'ls', timeout: 1.5 }, undefined, '/w'), { command: 'ls', cwd: '/w', timeout_ms: 1500 });
  assert.deepEqual(builtinArguments('write', { path: '~/x', content: 'c' }, '/w/sub', '/w'), { path: `${process.env.HOME}/x`, content: 'c' });
});

test('a call Pi 1.5 refuses is refused here for the same reason', () => {
  for (const timeout of [0, -1, 'abc', 'Infinity', 2147484]) {
    assert.throws(() => builtinArguments('bash', { command: 'ls', timeout }, undefined, '/w'), /invalid_request tool timeout/, String(timeout));
  }
  assert.throws(() => builtinArguments('read', { path: undefined }, undefined, '/w'), /invalid_request tool path/);
  assert.throws(() => builtinArguments('read', [], undefined, '/w'), /invalid_request tool arguments/);
  assert.throws(() => builtinArguments('grep', {}, undefined, '/w'), /unsupported_feature native grep tool/);
});

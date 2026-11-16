import test from 'node:test';
import assert from 'node:assert/strict';
import { canonicalToPi, piToCanonical } from '../lib/provider-context.mjs';
import { messageEntry } from '../lib/model-turns.mjs';
import { sessionEntryCopies, sessionEntryCopy, translateSessionEntries } from '../lib/session-mirror.mjs';
import { validateToolArguments } from '../shims/pi-1.5.0/ai/validation.mjs';

const call = (argument_error, arguments_json) => ({ Assistant: {
  model: 'model', protocol: 'open_ai_chat', content: [{ ToolCall: {
    id: 'call', name: 'read', arguments_json, async: false, argument_error,
  } }],
} });
const entry = (id, parent, message) => ({ id, parent, value: { type: 'message', ...message } });
const refusal = { code: -32601, message: /^unsupported_feature context tool call scheduling\/argument metadata:/ };

for (const [error, args] of [['schema_mismatch', '{"path":42}'], ['malformed', '{}']]) {
  test(`${error} remains an explicit refusal for observations and provider projections`, () => {
    const native = call(error, args), before = structuredClone(native);
    for (const options of [{}, { observation: true }, { replay: true }, { observation: true, replay: true }]) {
      assert.throws(() => canonicalToPi([native], new Map(), options), refusal);
    }
    const history = [entry('before', null, { User: { content: [{ Text: 'before' }] } }), entry('call', 'before', native)];
    assert.throws(() => translateSessionEntries(history, 'actual'), refusal);
    assert.throws(() => sessionEntryCopies(history, 'actual'), refusal);
    assert.throws(() => sessionEntryCopy(history, 'actual', 'call'), refusal);
    assert.throws(() => messageEntry(history[1], new Map()), refusal);
    assert.deepEqual(native, before);
    // Appending a failed call must invalidate a formerly readable cached view;
    // neither getter can fall back to the shorter successful translation.
    const growing = [history[0]];
    assert.equal(sessionEntryCopies(growing, 'actual').length, 1);
    growing.push(history[1]);
    assert.throws(() => sessionEntryCopies(growing, 'actual'), refusal);
    assert.throws(() => sessionEntryCopy(growing, 'actual', 'call'), refusal);
    assert.deepEqual(native, before);
  });
}

test('discarding a validation marker would make a copied observation replay as an unmarked call', () => {
  // This is a counterexample, not an allowed workaround: stripping native
  // failure metadata makes schema-invalid arguments indistinguishable from a
  // valid call, including after copying into another Pi API.
  const marked = call('schema_mismatch', '{"path":42}');
  const stripped = structuredClone(marked);
  delete stripped.Assistant.content[0].ToolCall.argument_error;
  const observed = canonicalToPi([stripped], new Map(), { observation: true });
  assert.deepEqual(observed[0].content[0].arguments, { path: 42 });
  const tool = { name: 'read', parameters: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] } };
  // Pi's coercion can even accept the arguments that native marked invalid.
  assert.deepEqual(validateToolArguments(tool, observed[0].content[0]), { path: '42' });
  const replayed = piToCanonical(structuredClone(observed));
  assert.equal(replayed[0].Assistant.content[0].ToolCall.argument_error, undefined);
  assert.notDeepEqual(replayed, [marked]);

  // The existing converter also refuses a Pi error marker rather than
  // silently turning it into a normal call, including after a plain copy.
  observed[0].content[0].argumentsError = 'native validation failed';
  for (const messages of [observed, structuredClone(observed)]) {
    assert.throws(() => piToCanonical(messages), { code: -32601, message: /^unsupported_feature Pi tool call.argumentsError:/ });
  }
});

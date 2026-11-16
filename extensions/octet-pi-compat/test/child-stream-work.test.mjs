import test from 'node:test';
import assert from 'node:assert/strict';
import { ChildEventProjection } from '../lib/child-events.mjs';

for (const channel of ['text', 'reasoning']) test(`child ${channel} observation work follows new bytes at N/10N/100N`, t => {
  const work = [];
  for (const count of [10, 100, 1000]) {
    let serialized = 0, cloned = 0;
    const stringify = JSON.stringify, clone = globalThis.structuredClone;
    JSON.stringify = (...args) => { const text = stringify(...args); serialized += Buffer.byteLength(text); return text; };
    globalThis.structuredClone = value => { cloned += Buffer.byteLength(stringify(value)); return clone(value); };
    const events = [], projection = new ChildEventProjection(event => events.push(event));
    try {
      projection.accept({ kind: 'run_started', timestamp: 1, message: 'prompt' });
      projection.accept({ kind: 'turn_started', timestamp: 2 });
      for (let i = 0; i < count; i++) projection.accept({ kind: 'output_delta', timestamp: 3, channel, text: 'x'.repeat(64) });
      t.diagnostic(`${count} deltas: serialized=${serialized} cloned=${cloned}`);
      work.push({ count, bytes: serialized + cloned });
      const updates = events.filter(event => event.type === 'message_update'), key = channel === 'text' ? 'text' : 'thinking';
      assert.equal(updates[0].message.content[0][key].length, 64);
      assert.equal(updates.at(-1).message.content[0][key].length, count * 64);
      updates[0].message.content[0][key] = 'callback mutation';
      assert.equal(projection.partial.content[0][key].length, count * 64);
    } finally { JSON.stringify = stringify; globalThis.structuredClone = clone; }
  }
  for (const { count, bytes } of work) assert.ok(bytes < 2048 + count * 512, `full child partial revisited at ${count}: ${bytes}`);
});

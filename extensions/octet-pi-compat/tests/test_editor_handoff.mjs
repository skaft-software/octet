import assert from "node:assert/strict";
import { setImmediate as tick } from "node:timers/promises";
import { test } from "node:test";
import { createEditorHandoff, EDITOR_HANDOFF_LIMITS } from "../editor_handoff.mjs";

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test("a delayed editor acknowledgement cannot replace a newer host observation", async (t) => {
  const reply = deferred();
  const handoff = createEditorHandoff({ isCurrent: () => true, request: () => reply.promise });
  t.after(() => handoff.dispose());
  const refresh = handoff.refresh();
  await tick();
  handoff.observe({ text: "new", revision: 2, focused: true });
  reply.resolve({ text: "old", revision: 1, focused: false });
  await refresh;
  assert.equal(handoff.getEditorText(), "new");
  assert.throws(() => handoff.observe({ text: "unsafe\x1b[31m", revision: 3, focused: true }), /bounded plain UTF-8/);
  assert.equal(handoff.getEditorText(), "new");
});

test("autocomplete uses exact UTF-8 suffixes and rejects stale and malformed requests", async (t) => {
  let disposed = 0;
  const handoff = createEditorHandoff({ isCurrent: () => true, request: async () => ({ accepted: true }) });
  t.after(() => handoff.dispose());
  const remove = handoff.addAutocompleteProvider(() => ({
    getSuggestions: (lines, row, column) => ({ prefix: lines[row].slice(0, column), items: [{ value: "éx", label: "choice" }] }),
    dispose() { disposed += 1; },
  }));
  await tick();
  handoff.observe({ text: "é", revision: 4, focused: true });
  assert.deepEqual(await handoff.complete({ text: "é", cursor: 2, revision: 4 }), {
    prefix: "é", items: [{ value: "éx", label: "choice" }],
  });
  assert.deepEqual(await handoff.complete({ text: "old", cursor: 3, revision: 3 }), { prefix: "", items: [] });
  await assert.rejects(handoff.complete({ text: "é", cursor: 1, revision: 4 }), /UTF-8 boundary/);
  await assert.rejects(handoff.complete({ text: "é", cursor: 2, revision: 4, owner: "forged" }), /invalid autocomplete request/);
  remove(); remove();
  assert.equal(disposed, 1);
  assert.deepEqual(await handoff.complete({ text: "é", cursor: 2, revision: 4 }), { prefix: "", items: [] });
});

test("owner disposal aborts an editor wait and prevents queued host mutations", async () => {
  const calls = [];
  const handoff = createEditorHandoff({
    isCurrent: () => true,
    request(method, params, { signal }) {
      calls.push({ method, params });
      return new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(signal.reason), { once: true });
      });
    },
  });
  const first = handoff.setEditorText("first");
  const second = handoff.setEditorText("must-not-send");
  await tick();
  handoff.dispose(); handoff.dispose();
  const results = await Promise.allSettled([first, second]);
  assert.deepEqual(results.map(({ status }) => status), ["rejected", "rejected"]);
  assert.deepEqual(calls, [{ method: "ui/editor", params: { operation: "set", text: "first" } }]);
  assert.throws(() => handoff.getEditorText(), /stale UI owner/);
});

test("cancelled non-cooperative completions retain their bounded slots until settlement", async (t) => {
  const held = [];
  const handoff = createEditorHandoff({ isCurrent: () => true, request: async () => ({ accepted: true }) });
  t.after(() => handoff.dispose());
  handoff.addAutocompleteProvider(() => ({
    getSuggestions() { const reply = deferred(); held.push(reply); return reply.promise; },
  }));
  await tick();
  const params = { text: "x", cursor: 1, revision: 0 };
  for (let index = 0; index < EDITOR_HANDOFF_LIMITS.concurrentCompletions; index += 1) {
    const controller = new AbortController();
    const request = handoff.complete(params, controller.signal);
    const rejected = assert.rejects(request, /fixture cancellation/);
    await tick();
    controller.abort(new Error("fixture cancellation"));
    await rejected;
  }
  await assert.rejects(handoff.complete(params), /query limit/);
  assert.equal(held.length, EDITOR_HANDOFF_LIMITS.concurrentCompletions);
  for (const reply of held) reply.resolve(null);
  await tick();
  const final = handoff.complete(params);
  await tick();
  held.at(-1).resolve({ prefix: "x", items: [{ value: "xy", label: "settled" }] });
  assert.deepEqual(await final, { prefix: "x", items: [{ value: "xy", label: "settled" }] });
});

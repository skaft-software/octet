import assert from "node:assert/strict";
import { test } from "node:test";

import {
  API_VERSION,
  MAX_EXTENSION_UI_ENTRIES,
  MAX_EXTENSION_UI_INDICATOR_FRAMES,
  MAX_EXTENSION_UI_LINES,
  MAX_EXTENSION_UI_TEXT_BYTES,
  SemanticUiAdmissionError,
  SemanticUiDisposedError,
  SemanticUiLimitError,
  SemanticUiStaleError,
  SemanticUiValidationError,
  createSemanticUiAdapter,
  sanitizeText,
} from "../semantic_ui.mjs";

const OWNER = Object.freeze({
  sessionId: "session-test",
  extensionInstanceId: "extension-test",
  generation: 4,
});

function makeAdapter(extra = {}) {
  const events = [];
  const admissions = [];
  const adapter = createSemanticUiAdapter({
    owner: OWNER,
    admit: (descriptor) => {
      admissions.push(descriptor);
      return descriptor.apiVersion === "0.2" && descriptor.feature === "semantic_ui";
    },
    emitContribution: (payload, context) => {
      events.push({ payload, context });
    },
    ...extra,
  });
  return { adapter, events, admissions };
}

function component(render, extra = {}) {
  return {
    render,
    invalidate() {},
    ...extra,
  };
}

test("admission is explicit, legacy API 0.2 scoped, and fail-closed", () => {
  assert.equal(API_VERSION, "0.2");
  assert.throws(
    () => createSemanticUiAdapter({ owner: OWNER, emit: () => {}, admit: () => false }),
    SemanticUiAdmissionError,
  );
  assert.throws(
    () => createSemanticUiAdapter({ owner: OWNER, emit: () => {} }),
    SemanticUiAdmissionError,
  );

  const { adapter, admissions, events } = makeAdapter();
  assert.equal(adapter.admitted, true);
  assert.equal(admissions.length, 1);
  assert.equal(admissions[0].apiVersion, "0.2");
  assert.equal(admissions[0].feature, "semantic_ui");
  assert.deepEqual(admissions[0].methods, ["status/contribution", "ui/contribution"]);
  assert.deepEqual(admissions[0].surfaces, ["status", "header", "footer"]);
  assert.equal(Object.isFrozen(admissions[0]), true);
  adapter.setStatus("legacy", "visible");
  assert.equal(events[0].context.apiVersion, "0.2");
  assert.equal(events[0].context.feature, "semantic_ui");
  for (const key of ["schema", "encoding", "capability", "capabilities"]) {
    assert.equal(Object.hasOwn(admissions[0], key), false);
    assert.equal(Object.hasOwn(events[0].context, key), false);
  }
});

test("canonical API 0.3 cannot admit legacy UI methods", () => {
  let admissions = 0;
  assert.throws(() => createSemanticUiAdapter({
    apiVersion: "0.3", owner: OWNER,
    admit: () => { admissions += 1; return true; },
    emit: () => assert.fail("canonical API 0.3 must not send a legacy contribution"),
  }), SemanticUiAdmissionError);
  assert.equal(admissions, 0);
});

test("text is bounded, neutral, and contains no terminal controls", () => {
  assert.equal(sanitizeText("a\tb\nc\r\u001b[31mred\u001b[0m"), "a\tb c red");
  assert.equal(sanitizeText("é😀", { ascii: true }), "??");
  assert.equal(new TextEncoder().encode(sanitizeText("😀😀", { maxBytes: 5 })).length, 4);
  assert.equal(sanitizeText("x".repeat(MAX_EXTENSION_UI_TEXT_BYTES + 100)).length, MAX_EXTENSION_UI_TEXT_BYTES);
  assert.throws(() => sanitizeText(42), SemanticUiValidationError);

  const { adapter, events } = makeAdapter();
  adapter.setStatus("safe.status", `\u001b]8;;https://evil.example\u0007click\u001b]8;;\u0007`);
  assert.equal(events[0].payload.text, "click");
  assert.equal(/\u001b|[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(events[0].payload.text), false);
  assert.equal(events[0].payload.style_role, "extension.pi.status");
});

test("ASCII projection consumes Unicode scalars without weakening terminal sanitization", () => {
  const text = "😀\u001b[31m𝄞\u001b[0m\nZ";
  assert.equal(sanitizeText(text), "😀𝄞 Z");
  assert.equal(sanitizeText(text, { ascii: true }), "?? Z");
  assert.equal(sanitizeText(text, { ascii: true, maxBytes: 3 }), "?? ");
  assert.equal(sanitizeText(text, { maxBytes: 7 }), "😀");
});

test("status, header, footer, and widget contributions remain data-only", () => {
  const { adapter, events } = makeAdapter();
  const calls = { header: 0, footer: 0, invalidates: 0, disposes: 0 };
  const header = (tui, theme) => {
    assert.equal(tui.width, 80);
    assert.equal(theme.noColor, true);
    calls.header += 1;
    return component((width) => [`header-${width}`], {
      wantsKeyRelease: true,
      invalidate() { calls.invalidates += 1; },
      dispose() { calls.disposes += 1; },
    });
  };
  const footer = (tui, theme, provider) => {
    assert.equal(tui.width, 80);
    assert.equal(theme.supportsColor, false);
    assert.equal(provider.getGitBranch(), null);
    calls.footer += 1;
    return component((width) => [`footer-${width}`], {
      invalidate() { calls.invalidates += 1; },
      dispose() { calls.disposes += 1; },
    });
  };

  adapter.setHeader(header);
  adapter.setFooter(footer);
  adapter.setWidget("lines", ["one", "two"], { placement: "belowEditor" });
  assert.equal(calls.header, 1);
  assert.equal(calls.footer, 1);
  assert.equal(adapter.wantsKeyRelease("header"), true);
  assert.equal(adapter.wantsKeyRelease("footer"), false);

  const surfaces = events.filter(({ context }) => context.method === "status/contribution");
  assert.deepEqual(surfaces.map(({ payload }) => payload.surface), ["header", "footer"]);
  assert.deepEqual(events.find(({ payload }) => payload.key === "lines").payload.lines, ["one", "two"]);
  for (const event of events) {
    assert.equal(Object.values(event.payload).some((value) => typeof value === "function"), false);
    assert.deepEqual(event.context.owner, {
      session_id: "session-test",
      extension_instance_id: "extension-test",
      process_generation: 4,
    });
  }

  adapter.resize(40);
  adapter.invalidate();
  assert.equal(calls.invalidates, 4);
  assert.equal(adapter.snapshot().header[0], "header-40");
  assert.equal(adapter.snapshot().footer[0], "footer-40");

  adapter.setHeader(null);
  adapter.setFooter(null);
  assert.equal(calls.disposes, 2);
  assert.deepEqual(events.at(-1).payload, {
    surface: "footer",
    text: "",
    style_role: "extension.pi.status",
    priority: 0,
  });
});

test("component render, input, wantsKeyRelease, invalidate, resize, and disposal are fenced", () => {
  const { adapter, events } = makeAdapter();
  const calls = { widths: [], input: [], invalidates: 0, disposes: 0 };
  const widget = (tui, theme) => {
    assert.equal(theme.noColor, true);
    return {
      wantsKeyRelease: true,
      render(width) {
        calls.widths.push(width);
        return [`value-${width}`];
      },
      invalidate() { calls.invalidates += 1; },
      handleInput(input) { calls.input.push(input); return "handled"; },
      dispose() { calls.disposes += 1; },
    };
  };
  adapter.setWidget("interactive", widget);
  assert.equal(adapter.wantsKeyRelease("interactive"), true);
  assert.equal(adapter.handleInput("interactive", "\u001b[Ax"), "handled");
  assert.deepEqual(calls.input, ["x"]);
  adapter.resize(25);
  assert.deepEqual(calls.widths, [80, 80, 25]);
  assert.equal(calls.invalidates, 2);
  adapter.setWidget("interactive", null);
  assert.equal(calls.disposes, 1);
  assert.equal(events.at(-1).payload.lines, null);
});

test("working metadata honors host bounds and reduced-motion policy", () => {
  const { adapter, events } = makeAdapter({ reducedMotion: true });
  const frames = Array.from({ length: MAX_EXTENSION_UI_INDICATOR_FRAMES + 5 }, (_, index) => `f${index}`);
  adapter.setWorkingMessage("working\nnow");
  adapter.setWorkingVisible(true);
  adapter.setWorkingIndicator(frames, 100);
  const contribution = events.at(-1).payload;
  assert.equal(contribution.kind, "working");
  assert.deepEqual(contribution.frames, ["f0"]);
  assert.equal(contribution.interval_ms, null);
  assert.equal(contribution.message, "working now");
  assert.equal(contribution.visible, true);

  const normal = makeAdapter().adapter;
  assert.throws(() => normal.setWorkingIndicator(["a"], 15), SemanticUiValidationError);
  assert.throws(() => normal.setWorkingIndicator(["a"], 10_001), SemanticUiValidationError);
});

test("entry and renderer bounds are enforced without merging renderer kinds", () => {
  const { adapter } = makeAdapter();
  for (let index = 0; index < MAX_EXTENSION_UI_LINES + 5; index += 1) {
    // The adapter emits only the bounded prefix, never an unbounded render.
    adapter.setWidget("bounded", Array.from({ length: index + 1 }, () => "line"));
  }
  assert.equal(adapter.snapshot().widgets.bounded.lines.length, MAX_EXTENSION_UI_LINES);

  let messagePayload;
  let entryPayload;
  adapter.registerMessageRenderer("message.type", (payload, options, theme) => {
    messagePayload = { payload, options, theme };
    return component(() => ["message\u001b[31m"]);
  });
  adapter.registerEntryRenderer("entry.type", (payload) => {
    entryPayload = payload;
    return component(() => ["entry"]);
  });
  assert.deepEqual(adapter.renderMessageRenderer("message.type", { text: "a\u001b[31mb" }), ["message"]);
  assert.deepEqual(adapter.renderEntryRenderer("entry.type", { text: "entry" }), ["entry"]);
  assert.equal(messagePayload.payload.text, "ab");
  assert.equal(entryPayload.text, "entry");
  assert.deepEqual(adapter.rendererTypes("message"), ["message.type"]);
  assert.deepEqual(adapter.rendererTypes("entry"), ["entry.type"]);
});

test("entry capacity is shared by keyed statuses and widgets", () => {
  const { adapter } = makeAdapter();
  for (let index = 0; index < MAX_EXTENSION_UI_ENTRIES; index += 1) {
    adapter.setStatus(`s${index}`, "status");
  }
  assert.throws(() => adapter.setStatus("overflow", "x"), SemanticUiLimitError);
  assert.throws(() => adapter.setWidget("overflow", ["x"]), SemanticUiLimitError);
  adapter.setStatus("s0", null);
  adapter.setWidget("now-available", ["x"]);
  assert.equal(adapter.snapshot().widgets["now-available"].lines[0], "x");
});

test("failed component replacement preserves the old widget and disposes each generation once", () => {
  const { adapter } = makeAdapter();
  let requestOldRender;
  let oldDisposals = 0;
  let failedDisposals = 0;
  adapter.setWidget("owned", (tui) => {
    requestOldRender = () => tui.requestRender();
    return component(() => ["old"], { dispose() { oldDisposals += 1; } });
  });
  assert.throws(() => adapter.setWidget("owned", () => component(() => {
    throw new Error("broken render");
  }, { dispose() { failedDisposals += 1; } })), /render/);
  assert.deepEqual(adapter.snapshot().widgets.owned.lines, ["old"]);
  assert.equal(oldDisposals, 0);
  assert.equal(failedDisposals, 1);
  adapter.setWidget("owned", ["new"]);
  assert.equal(oldDisposals, 1);
  assert.throws(requestOldRender, SemanticUiDisposedError);
  adapter.dispose(); adapter.dispose();
  assert.equal(oldDisposals, 1);
  assert.equal(failedDisposals, 1);
});

test("invalid constructed components are disposed before their error escapes", () => {
  const { adapter } = makeAdapter();
  let disposals = 0;
  assert.throws(() => adapter.setHeader(() => ({
    render: () => ["never accepted"], dispose() { disposals += 1; },
  })), /invalidate/);
  assert.equal(disposals, 1);
  assert.equal(adapter.snapshot().header, null);
  adapter.dispose();
  assert.equal(disposals, 1);
});

test("stale owners and cancellation dispose before later emission", () => {
  let current = true;
  const stale = makeAdapter({ isCurrent: () => current });
  stale.adapter.setStatus("before", "ok");
  current = false;
  assert.throws(() => stale.adapter.setStatus("after", "no"), SemanticUiStaleError);
  assert.equal(stale.adapter.disposed, true);
  assert.throws(() => stale.adapter.setStatus("again", "no"), SemanticUiDisposedError);
  assert.equal(stale.events.length, 1);

  const controller = new AbortController();
  const cancelled = makeAdapter({ signal: controller.signal });
  controller.abort();
  assert.equal(cancelled.adapter.disposed, true);
  assert.throws(() => cancelled.adapter.setStatus("cancelled", "no"), SemanticUiDisposedError);
});

test("dispose is idempotent and unregister closures cannot revive a generation", () => {
  const { adapter } = makeAdapter();
  const unregister = adapter.registerMessageRenderer("once", () => component(() => ["ok"]));
  assert.deepEqual(adapter.rendererTypes("message"), ["once"]);
  unregister();
  unregister();
  assert.deepEqual(adapter.rendererTypes("message"), []);
  adapter.dispose();
  adapter.dispose();
  assert.equal(adapter.snapshot().disposed, true);
  assert.throws(() => adapter.registerEntryRenderer("late", () => component(() => ["no"])), SemanticUiDisposedError);
});

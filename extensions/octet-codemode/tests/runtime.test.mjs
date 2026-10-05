import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, readFile, rm, symlink, writeFile, stat } from "node:fs/promises";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PassThrough } from "node:stream";
import { Executor, validateArguments } from "../sandbox.mjs";
import { Runtime, negotiate } from "../runtime.mjs";
import { readHostJsonFile } from "../host-files.mjs";
import { validateContext, discovery } from "../discovery.mjs";
import { Scratch, formatResult } from "../output.mjs";
import { FRAME_BYTES, OUTPUT_BYTES } from "../common.mjs";

const parameters = { type: "object", properties: { value: { type: "integer" } }, required: ["value"], additionalProperties: false };
const tools = [
  { name: "first", description: "Read numbers", parameters, output_schema: { type: "object" } },
  { name: "second", description: "Double a number", parameters, output_schema: { type: "integer" } },
  { name: "plain", description: "Text result", parameters },
];
const context = (store = {}, catalog = tools) => ({ tools: catalog, store, limits: { timeout_ms: 30000, max_calls: 256 } });
const offer = (flags = []) => ({ api_version: "0.4", octet_version: "0.8.2", extension: { name: "octet-codemode", version: "0.8.2" },
  protocol: { version: "0.4", required_features: ["request_cancellation", "content_parts"],
    optional_features: ["tool_composition_v1", "request_progress", "artifacts"], limits: { max_concurrent_requests: 8 } }, flag_values: flags });
const text = (result) => result.content.filter((part) => part.type === "text").map((part) => part.text).join("\n");
async function scratch(t) {
  const path = await mkdtemp(join(tmpdir(), "octet-codemode-test-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}
async function fixture(t, override = {}) {
  const root = await scratch(t);
  const commits = [], calls = [];
  let store = { previous: 5 };
  const rpc = { async request(method, params, signal) {
    assert(!signal?.aborted, "cancelled calls cannot be sent");
    if (override.request) return await override.request(method, params, signal);
    if (method === "composition/context") return context(store);
    if (method === "composition/call") {
      calls.push(params);
      return { value: params.name === "first" ? { numbers: [1, 2, 3], unicode: "🌱" } : params.name === "second" ? params.arguments.value * 2 : '{"not":"parsed"}' };
    }
    if (method === "composition/store") {
      commits.push(params);
      store = { ...store, ...params.set };
      for (const key of params.delete) delete store[key];
      return {};
    }
    throw new Error(`Unexpected reverse method: ${method}`);
  } };
  const executor = new Executor({ rpc, scratch: root, features: override.features ?? new Set() });
  return { root, calls, commits, execute: (code, signal = new AbortController().signal) => executor.execute(1, { code }, signal), executor };
}

test("negotiates exact API 0.4, grammar, mode and host inline bound", () => {
  const selection = negotiate(offer([{ name: "codemode-mode", value: "only" }, { name: "codemode-inline-budget", value: 16000 }]));
  assert.deepEqual(selection.result.tools[0].composition, { mode: "only", inline_budget: 16000 });
  assert.equal(selection.result.tools[0].constrained_sampling.type, "grammar");
  assert.equal(selection.result.protocol.limits.max_concurrent_requests, 8);
  assert.equal(negotiate(offer()).result.tools[0].composition.mode, "on");
  for (const bad of [
    { ...offer(), api_version: "0.3" },
    { ...offer(), protocol: { ...offer().protocol, optional_features: [] } },
    offer([{ name: "codemode-mode", value: "off" }]),
    offer([{ name: "codemode-inline-budget", value: 16001 }]),
    offer([{ name: "codemode-mode", value: "on" }, { name: "codemode-mode", value: "only" }]),
  ]) assert.throws(() => negotiate(bad), /Invalid params/);
});

test("accepts native optional resource and bulk offers without selecting them", () => {
  const init = offer();
  init.protocol.optional_features.push("resource_refs_v1", "bulk_objects_v1");
  init.protocol.limits.resource_refs_v1 = { max_records: 256, max_registrations_per_parent: 32 };
  init.protocol.bulk_objects_v1 = { profile: "local-file.v1", transfer_directory: "/unused", limits: {} };
  const selection = negotiate(init);
  assert.equal(selection.maxConcurrent, 8);
  assert(!selection.features.has("resource_refs_v1"));
  assert(!selection.features.has("bulk_objects_v1"));
  assert.deepEqual(selection.result.protocol.limits, { max_concurrent_requests: 8 });
  assert.throws(() => negotiate({ ...init, protocol: { ...init.protocol, unknown: true } }), /Invalid params/);
  assert.throws(() => negotiate({ ...init, protocol: { ...init.protocol,
    limits: { ...init.protocol.limits, max_concurrent_requests: 3 } } }), /Invalid params/);
});

test("source and frozen context boundaries reject malformed input", () => {
  for (const bad of [{ code: "" }, { code: "return 1", extra: true }, { code: "x".repeat(65537) },
    { code: '// @options: {"timeout_ms":0}\nreturn 1' }, { code: '// @options: {"unknown":1}\nreturn 1' }]) {
    assert.throws(() => validateArguments(bad));
  }
  assert.equal(validateArguments({ code: '// @options: {"max_output_tokens":0}\nreturn 1' }).options.maxOutputTokens, 0);
  for (const bad of [{ ...context(), limits: { timeout_ms: 30001, max_calls: 256 } },
    context({}, [...tools, tools[0]]), context({}, [{ ...tools[0], output_schema: { type: "unknown" } }]),
    { ...context(), resource_owner: "guest override" }]) assert.throws(() => validateContext(bad));
});

test("real offline Pi/WASM chains JSON while schema-less results stay text", async (t) => {
  const f = await fixture(t);
  const result = await f.execute('const a=await tools.first({value:2}); const n=await tools.second({value:a.numbers.length}); text(a.unicode); return [n, typeof await tools.plain({value:0})];');
  assert.equal(result.is_error, false, text(result));
  assert.match(text(result), /🌱/);
  assert.match(text(result), /\[6,"string"\]/);
  assert.deepEqual(f.calls.map((call) => call.name), ["first", "second", "plain"]);
  assert(f.calls.every((call) => call.parent_request_id === 1));
});

test("guest has no Node, filesystem, network, timers, modules or models", async (t) => {
  const f = await fixture(t);
  const result = await f.execute('return [typeof process,typeof require,typeof fetch,typeof setTimeout,typeof Buffer,typeof models].join(",");');
  assert.equal(result.is_error, false, text(result));
  assert.match(text(result), /undefined,undefined,undefined,undefined,undefined,undefined/);
  assert.equal(f.calls.length, 0);
});

test("store/load survives successful scripts; failures keep partial output, not writes", async (t) => {
  const f = await fixture(t);
  const success = await f.execute('store("answer",load("previous")+1); return load("answer");');
  assert.equal(success.is_error, false, text(success));
  assert.equal(f.commits[0].set.answer, 6);
  const failure = await f.execute('text("partial");store("answer",99);throw new Error("oops");');
  assert.equal(failure.is_error, true);
  assert.match(text(failure), /partial[\s\S]*oops/);
  assert.equal(f.commits.length, 1);
  assert.match(text(await f.execute('return load("answer");')), /\n6$/);
});

test("tool errors reject inside JS and can be handled with allSettled", async (t) => {
  const f = await fixture(t, { request: async (method) => {
    if (method === "composition/context") return context();
    throw new Error("Host denied nested effect");
  } });
  const result = await f.execute('const rows=await Promise.allSettled([tools.first({value:1}),tools.second({value:2})]);return rows.map(row=>row.status);');
  assert.equal(result.is_error, false, text(result));
  assert.match(text(result), /\["rejected","rejected"\]/);
  assert.equal(result.metadata.call_count, 2);
});

test("hard infinite-loop timeout terminates the real VM and prevents store commit", async (t) => {
  const f = await fixture(t);
  const result = await f.execute('// @options: {"timeout_ms":300}\ntext("before timeout");store("bad",1);while(true){}');
  assert.equal(result.is_error, true);
  assert.match(text(result), /timed out|timeout/i);
  assert.match(text(result), /before timeout/);
  assert.equal(f.commits.length, 0);
  assert.equal(f.executor.running, false);
});

test("guest memory and output capture ceilings fail before store commit", async (t) => {
  const f = await fixture(t);
  const memory = await f.execute('store("bad",1);const rows=new Array(40000000).fill(1);return rows.length;');
  assert.equal(memory.is_error, true);
  assert.match(text(memory), /memory|alloc/i);
  const output = await f.execute('store("bad",1);text("x".repeat(17*1024*1024));');
  assert.equal(output.is_error, true);
  assert.match(text(output), /capture limit/);
  assert.equal(f.commits.length, 0);
});

test("parent cancellation terminates the VM with no store commit", async (t) => {
  const f = await fixture(t);
  const controller = new AbortController();
  const execution = f.execute('store("bad",1);while(true){}', controller.signal);
  setTimeout(() => controller.abort(), 150);
  await assert.rejects(execution, /cancelled/i);
  assert.equal(f.commits.length, 0);
  assert.equal(f.executor.running, false);
});

test("four-call limiter cancels queued unawaited work before host dispatch", async (t) => {
  let dispatched = 0;
  const f = await fixture(t, { request: async (method, _, signal) => {
    if (method === "composition/context") return context();
    dispatched++;
    return await new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("cancelled")), { once: true }));
  } });
  const result = await f.execute('for(let i=0;i<20;i++)tools.first({value:i});return "done";');
  assert.equal(result.is_error, false, text(result));
  assert(dispatched <= 4, `dispatched ${dispatched} unawaited calls`);
  assert(result.metadata.calls.every((call) => call.status === "cancelled"));
});

test("a stricter host call bound cannot be raised by the guest", async (t) => {
  let count = 0;
  const f = await fixture(t, { request: async (method) => {
    if (method === "composition/context") return { ...context(), limits: { timeout_ms: 30000, max_calls: 2 } };
    count++; return { value: {} };
  } });
  const result = await f.execute('await tools.first({value:1});await tools.first({value:2});await tools.first({value:3});');
  assert.equal(result.is_error, true);
  assert.match(text(result), /exceeded 2 nested tool calls/);
  assert.equal(count, 2);
});

test("Pi aliases and deferred discovery resolve qualified names", async (t) => {
  const catalog = [{ ...tools[0], name: "mcp__github__list-issues", description: "Find repository issues" }];
  const globals = discovery(catalog).globals;
  assert.equal(globals[0].execute(["repository issues", { namespace: "github" }])[0].name, "mcp__github__list_issues");
  assert.match(globals[1].execute(["mcp__github__list_issues"]), /list_issues/);
  assert.deepEqual(globals[2].execute(["github"]).tools, ["mcp__github__list_issues"]);
  const f = await fixture(t, { request: async (method, params) => method === "composition/context" ? context({}, catalog) : { value: { name: params.name } } });
  const result = await f.execute('text(ALL_TOOLS);return await tools.mcp__github__list_issues({value:1});');
  assert.equal(result.is_error, false, text(result));
  assert.match(text(result), /mcp__github__list-issues/);
});

test("truncation retains UTF-8 full output privately; zero token budget is valid", async (t) => {
  const root = await scratch(t);
  const output = "🌱".repeat(20000);
  const result = await formatResult({ ok: true, items: [{ type: "text", text: output }], calls: [], timeoutMs: 25000, maxCalls: 256, maxTokens: 0, wallMs: 1, scratch: new Scratch(root) });
  assert(result.metadata.output_truncated);
  assert(Buffer.byteLength(text(result)) <= OUTPUT_BYTES);
  assert.equal(await readFile(result.metadata.full_output_path, "utf8"), output);
  if (process.platform !== "win32") assert.equal((await stat(result.metadata.full_output_path)).mode & 0o777, 0o600);
});

test("images use negotiated host artifacts and failed publication prevents store writes", async (t) => {
  const image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a0n8AAAAASUVORK5CYII=";
  const requests = [];
  const f = await fixture(t, { features: new Set(["artifacts"]), request: async (method, params) => {
    if (method === "composition/context") return context();
    requests.push({ method, params });
    return method === "artifact/publish" ? { artifact_id: "artifact:test" } : {};
  } });
  const result = await f.execute(`image(${JSON.stringify(image)});store("image",1);`);
  assert.equal(result.is_error, false, text(result));
  assert.equal(result.content[1].artifact_id, "artifact:test");
  assert.deepEqual(requests.map((row) => row.method), ["artifact/publish", "composition/store"]);
  const unavailable = await fixture(t);
  const failed = await unavailable.execute(`image(${JSON.stringify(image)});store("bad",1);`);
  assert.equal(failed.is_error, true);
  assert.match(text(failed), /artifacts feature/);
  assert.equal(unavailable.commits.length, 0);
});

async function hostFile(root, value, name = "host.json") {
  const data = Buffer.from(JSON.stringify(value));
  await writeFile(join(root, name), data, { mode: 0o600 });
  return { path: name, bytes: data.length, sha256: createHash("sha256").update(data).digest("hex") };
}
test("oversized host JSON sidecars preserve Unicode and are unlinked after use", async (t) => {
  const root = await scratch(t), value = { large: "🌱".repeat(300000) };
  const reference = await hostFile(root, value);
  assert(reference.bytes > FRAME_BYTES);
  assert.deepEqual(await readHostJsonFile(reference, root), value);
  await assert.rejects(stat(join(root, reference.path)), { code: "ENOENT" });
});
test("sidecar traversal, size, digest and symlink boundaries fail closed", async (t) => {
  const root = await scratch(t);
  for (const mutate of [(ref) => ({ ...ref, bytes: ref.bytes + 1 }), (ref) => ({ ...ref, sha256: "0".repeat(64) })]) {
    const ref = await hostFile(root, { value: 1 });
    await assert.rejects(readHostJsonFile(mutate(ref), root), /mismatch/);
    await assert.rejects(stat(join(root, ref.path)), { code: "ENOENT" });
  }
  const ref = await hostFile(root, { keep: true });
  await assert.rejects(readHostJsonFile({ ...ref, path: "../host.json" }, root), /Invalid/);
  await symlink(join(root, ref.path), join(root, "linked.json"));
  await assert.rejects(readHostJsonFile({ ...ref, path: "linked.json" }, root), /non-symlink/);
  assert.deepEqual(JSON.parse(await readFile(join(root, ref.path))), { keep: true });
});

async function transport(t, initialize = true) {
  const root = await scratch(t), input = new PassThrough(), output = new PassThrough(), diagnostics = new PassThrough();
  const messages = [];
  let buffer = "";
  output.on("data", (data) => {
    buffer += data.toString();
    for (;;) { const at = buffer.indexOf("\n"); if (at < 0) break; messages.push(JSON.parse(buffer.slice(0, at))); buffer = buffer.slice(at + 1); }
  });
  const runtime = new Runtime({ input, output, diagnostics, scratch: root });
  const done = runtime.run();
  t.after(async () => { await runtime.stop(true); input.destroy(); output.destroy(); });
  const send = (id, method, params) => input.write(`${JSON.stringify({ jsonrpc: "2.0", ...(id === undefined ? {} : { id }), method, params })}\n`);
  async function next(predicate) {
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      const match = messages.find(predicate);
      if (match) return match;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    throw new Error(`Missing frame: ${JSON.stringify(messages)}`);
  }
  if (initialize) { send(1, "initialize", offer()); assert((await next((row) => row.id === 1)).result); }
  return { runtime, input, messages, send, next, done };
}
test("real stdio-shaped transport composes only under the numeric parent", async (t) => {
  const f = await transport(t);
  f.send(2, "tool/call", { name: "codemode", arguments: { code: "return await tools.second({value:3});" }, context: {} });
  const ctx = await f.next((row) => row.method === "composition/context");
  assert.equal(ctx.params.parent_request_id, 2);
  f.input.write(`${JSON.stringify({ jsonrpc: "2.0", id: ctx.id, result: context() })}\n`);
  const call = await f.next((row) => row.method === "composition/call");
  assert.deepEqual(call.params, { parent_request_id: 2, name: "second", arguments: { value: 3 } });
  f.input.write(`${JSON.stringify({ jsonrpc: "2.0", id: call.id, result: { value: 6 } })}\n`);
  assert.match(text((await f.next((row) => row.id === 2)).result), /\n6$/);
});
test("commands and menu need no composition authority", async (t) => {
  const f = await transport(t);
  f.send(2, "command/execute", { name: "codemode", arguments: ["status"], context: {} });
  assert.match((await f.next((row) => row.id === 2)).result.text, /Models helpers are unavailable/);
  f.send(3, "menu/collect", { context: {} });
  assert.equal((await f.next((row) => row.id === 3)).result.items.length, 2);
  assert(!f.messages.some((row) => row.method?.startsWith("composition/")));
});
test("transport cancellation revokes reverse waiters and answers once", async (t) => {
  const f = await transport(t);
  f.send(2, "tool/call", { name: "codemode", arguments: { code: "return 1;" }, context: {} });
  const ctx = await f.next((row) => row.method === "composition/context");
  f.send(undefined, "$/cancelRequest", { id: 2 });
  assert.equal((await f.next((row) => row.id === 2)).error.code, -32800);
  await f.next((row) => row.method === "$/cancelRequest" && row.params.id === ctx.id);
  f.input.write(`${JSON.stringify({ jsonrpc: "2.0", id: ctx.id, result: context() })}\n`);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(f.messages.filter((row) => row.id === 2).length, 1);
  assert.equal(f.runtime.pending.size, 0);
});
test("malformed and duplicate requests fail without dispatch", async (t) => {
  const f = await transport(t);
  f.send(2, "tool/call", { name: "codemode", arguments: { code: "return 1", unknown: true }, context: {} });
  assert.equal((await f.next((row) => row.id === 2)).error.code, -32602);
  f.send(1, "initialize", offer());
  await f.next((row) => row.id === 1 && row.error);
  assert(!f.messages.some((row) => row.method?.startsWith("composition/")));
});
test("graceful shutdown drains active parents and stdin EOF stops the runtime", async (t) => {
  const f = await transport(t);
  f.send(2, "tool/call", { name: "codemode", arguments: { code: "return 1;" }, context: {} });
  await f.next((row) => row.method === "composition/context");
  f.send(3, "shutdown", {});
  assert.equal((await f.next((row) => row.id === 2)).error.code, -32800);
  assert.deepEqual((await f.next((row) => row.id === 3)).result, {});
  assert.equal(await f.done, 0);
  const eof = await transport(t, false);
  eof.input.end();
  assert.equal(await eof.done, 0);
});
test("oversized transport frames terminate before parsing or dispatch", async (t) => {
  const f = await transport(t, false);
  f.input.write(Buffer.alloc(FRAME_BYTES + 1, 32));
  assert.equal(await f.done, 1);
  assert.equal(f.runtime.pending.size, 0);
});

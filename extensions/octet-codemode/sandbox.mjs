import { fileURLToPath } from "node:url";
import { CodemodeSandbox, loadQuickJSWasm, parseCodemodeSource } from "./vendor/pi-codemode/dist/index.js";
import { compositionContext, compositionValue } from "./host-files.mjs";
import { discovery, validateContext } from "./discovery.mjs";
import { Scratch, formatResult } from "./output.mjs";
import { MAX_CALLS, LOCAL_TIMEOUT_MS, VM_HEAP_BYTES, RpcError, cancelled, exactKeys, has, isObject, params, preview, head, messageOf } from "./common.mjs";

// One script per extension, four admitted nested calls per script. Queueing is
// cancellation-aware; an unawaited queued call never reaches the host.
export class Limiter {
  constructor(limit) { this.limit = limit; this.active = 0; this.queue = []; }
  async run(signal, operation) {
    cancelled(signal);
    if (this.active >= this.limit) await new Promise((resolve, reject) => {
      const entry = { signal, resolve, reject };
      entry.abort = () => { const index = this.queue.indexOf(entry); if (index >= 0) this.queue.splice(index, 1); reject(new RpcError(-32800, "Request cancelled")); };
      signal?.addEventListener("abort", entry.abort, { once: true });
      this.queue.push(entry);
    });
    // A woken waiter owns the slot. Increment happens before waking it below.
    else this.active++;
    try { cancelled(signal); return await operation(); }
    finally {
      const entry = this.queue.shift();
      if (entry) { entry.signal?.removeEventListener("abort", entry.abort); entry.resolve(); }
      else this.active--;
    }
  }
}

export const CODE_SCHEMA = { type: "object", properties: { code: { type: "string", maxLength: 65536 } }, required: ["code"], additionalProperties: false };
export function validateArguments(arguments_) {
  params(exactKeys(arguments_, ["code"]) && typeof arguments_.code === "string" && [...arguments_.code].length <= 65536,
    "codemode accepts only {code: string}, at most 65536 Unicode characters");
  try { return parseCodemodeSource(arguments_.code); }
  catch (error) { throw new RpcError(-32602, `Invalid params: ${messageOf(error)}`); }
}
const wasmPath = fileURLToPath(new URL("./vendor/quickjs-wasi/quickjs.wasm", import.meta.url));

export class Executor {
  constructor({ rpc, scratch, features = new Set(), progress = () => {} }) {
    this.rpc = rpc; this.scratch = new Scratch(scratch); this.scratchRoot = scratch;
    this.features = features; this.progress = progress; this.serial = new Limiter(1);
    this.running = false;
  }
  execute(parent, arguments_, parentSignal) {
    const source = validateArguments(arguments_);
    return this.serial.run(parentSignal, () => this.run(parent, source, parentSignal));
  }
  async run(parent, source, parentSignal) {
    cancelled(parentSignal);
    this.running = true;
    const startedAt = performance.now();
    const timeout = Math.min(source.options.timeoutMs ?? LOCAL_TIMEOUT_MS, LOCAL_TIMEOUT_MS);
    const deadline = new AbortController();
    const signal = AbortSignal.any([parentSignal, deadline.signal]);
    let timeoutMs = timeout;
    const timer = setTimeout(() => deadline.abort(new Error(`Script timed out after ${timeoutMs} ms (local cap 25000 ms; host cap 30000 ms)`)), timeout);
    let sandbox;
    const calls = [];
    let maxCalls = MAX_CALLS;
    let result;
    let items = [];
    let error;
    let ok = false;
    try {
      this.progress(parent, "Preparing frozen tool snapshot");
      const context = validateContext(await compositionContext(await this.rpc.request("composition/context", { parent_request_id: parent }, signal), this.scratchRoot));
      cancelled(signal);
      timeoutMs = Math.min(timeout, context.limits.timeout_ms);
      // A stricter host deadline applies to setup, sandbox, and result/store
      // finalization, not a fresh timer for each nested operation.
      clearTimeout(timer);
      const remaining = Math.max(1, timeoutMs - (performance.now() - startedAt));
      const hostTimer = setTimeout(() => deadline.abort(new Error(`Script timed out after ${timeoutMs} ms`)), remaining);
      this.deadlineTimer = hostTimer;
      maxCalls = Math.min(MAX_CALLS, context.limits.max_calls);
      const nested = new Limiter(4);
      const { globals, samples } = discovery(context.tools);
      let callCount = 0;
      const tools = context.tools.map((tool) => ({ name: tool.name, description: samples.get(tool.name),
        execute: async (args, { signal: toolSignal }) => {
          const callSignal = AbortSignal.any([signal, toolSignal]);
          cancelled(callSignal);
          if (++callCount > maxCalls) throw new Error(`Script exceeded ${maxCalls} nested tool calls`);
          const record = { id: `${parent}/${callCount}`, name: tool.name, args: preview(args), status: "running" };
          calls.push(record);
          const calledAt = performance.now();
          try {
            if (!isObject(args)) throw new Error(`Tool ${tool.name} arguments must be an object`);
            return await nested.run(callSignal, async () => {
              const response = await this.rpc.request("composition/call", { parent_request_id: parent, name: tool.name, arguments: args }, callSignal);
              const value = await compositionValue(response, this.scratchRoot);
              cancelled(callSignal);
              if (!has(tool, "output_schema") && typeof value !== "string") throw new Error(`Host returned a non-text value for schema-less tool ${tool.name}`);
              record.status = "ok";
              return value;
            });
          } catch (error) {
            record.status = callSignal.aborted ? "cancelled" : "error";
            record.error = head(messageOf(error), 500);
            throw error;
          } finally { record.durationMs = performance.now() - calledAt; }
        } }));
      sandbox = new CodemodeSandbox({ tools, globals, timeoutMs: remaining, memoryLimitBytes: VM_HEAP_BYTES,
        wasm: loadQuickJSWasm(wasmPath), workerUrl: new URL("./worker.mjs", import.meta.url) });
      this.progress(parent, `Running JavaScript (deadline ${timeoutMs} ms, up to ${maxCalls} calls)`);
      result = await sandbox.execute(source.code, { signal, store: context.store, timeoutMs: remaining });
      await sandbox.close();
      cancelled(parentSignal);
      for (const call of calls) if (call.status === "running") call.status = "cancelled";
      ok = result.ok && !signal.aborted;
      error = result.ok ? undefined : result.error;
      if (deadline.signal.aborted) error = { kind: "timeout", message: messageOf(deadline.signal.reason) };
      items = result.output.filter((item) => item.type === "text").map((item) => ({ ...item }));
      if (result.ok && result.value !== undefined) items.push({ type: "text", text: typeof result.value === "string" ? result.value : JSON.stringify(result.value) });
      // The host verifies media bytes/digest/ownership. Failed scripts retain
      // partial images too; cancellation/deadline never starts publication.
      for (const item of result.output.filter((item) => item.type === "image")) {
        if (signal.aborted) break;
        try { items.push(await this.scratch.publish(item, parent, signal, this.rpc, this.features)); }
        catch (failure) {
          cancelled(parentSignal); ok = false;
          error = { kind: "artifact", message: `Image publication failed: ${messageOf(failure)}` };
          break;
        }
      }
      if (ok) {
        cancelled(signal);
        const writes = result.storeWrites;
        if (Object.keys(writes.set).length || writes.delete.length) {
          this.progress(parent, "Persisting successful branch-scoped store writes");
          const committed = await this.rpc.request("composition/store", { parent_request_id: parent, set: writes.set, delete: writes.delete }, signal);
          if (!exactKeys(committed, [])) throw new Error("Invalid composition/store acknowledgement");
        }
      }
    } catch (failure) {
      cancelled(parentSignal);
      ok = false;
      error = { kind: deadline.signal.aborted ? "timeout" : "sandbox", message: deadline.signal.aborted ? messageOf(deadline.signal.reason) : messageOf(failure) };
    } finally {
      clearTimeout(timer); clearTimeout(this.deadlineTimer); this.deadlineTimer = undefined;
      await sandbox?.close(); this.running = false;
    }
    cancelled(parentSignal);
    if (signal.aborted) { ok = false; error = { kind: "timeout", message: messageOf(deadline.signal.reason) }; }
    for (const call of calls) if (call.status === "running") call.status = "cancelled";
    return await formatResult({ ok, items, error, calls, timeoutMs, maxCalls,
      maxTokens: source.options.maxOutputTokens ?? 10_000, wallMs: performance.now() - startedAt, scratch: this.scratch });
  }
}

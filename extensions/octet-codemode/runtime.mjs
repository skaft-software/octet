import { pathToFileURL } from "node:url";
import { CODEMODE_SOURCE_GRAMMAR } from "./vendor/pi-codemode/dist/index.js";
import { Executor, CODE_SCHEMA } from "./sandbox.mjs";
import { FRAME_BYTES, RpcError, cancelled, exactKeys, has, head, isObject, messageOf, params, validId, validateJson } from "./common.mjs";
import { readHostJsonFile } from "./host-files.mjs";

const REQUIRED = ["request_cancellation", "content_parts", "tool_composition_v1"];
const SUPPORTED = new Set([...REQUIRED, "request_progress", "artifacts"]);
const DESCRIPTION = `Run JavaScript (not TypeScript) as an async function body in Pi's QuickJS/WASM sandbox. Use top-level await and return; tools.<name>({arguments}) makes real host-brokered calls. Use Promise.allSettled to batch, chain calls, and filter large results before returning. No Node globals, filesystem, network, subprocesses, timers, imports, or models namespace.
Globals: tools, ALL_TOOLS, text(value), image(dataUrlOrImageBlock), exit(), console.log/info/warn/error/debug, store(key,value), load(key), searchTools(query,{limit?,namespace?}), describeTool(name), describeNamespace(name). Tool failures reject with their error text. Tools without output_schema return text, otherwise JSON. Pending/unawaited calls are cancelled, not undone. Store writes persist only after successful execution on the current session branch.
Optional first line: // @options: {"max_output_tokens":10000,"timeout_ms":25000}
Defaults: 10000 estimated output tokens; 25000 ms total local deadline (host maximum 30000 ms, lower host limits win); 256 calls, four concurrent nested calls, 256 MiB VM heap. Output retains its head/tail within 50 KiB; full truncated UTF-8 text is saved in private scratch. Discovery uses BM25 (default 8); aliases replace non-identifier characters with _, first collision wins. image() accepts base64 PNG/JPEG/GIF/WebP and requires negotiated host artifacts. Classifier/image operations may be provided by separately enabled tools; octet's chat-only host supplies no models helper.`;

export function negotiate(init) {
  params(isObject(init) && init.api_version === "0.4" && init.octet_version === "0.8.2" &&
    isObject(init.extension) && init.extension.name === "octet-codemode" && init.extension.version === "0.8.2",
    "requires API 0.4, octet 0.8.2 and octet-codemode 0.8.2");
  const offer = init.protocol;
  params(exactKeys(offer, ["version", "required_features", "optional_features", "limits"]) && offer.version === "0.4" &&
    Array.isArray(offer.required_features) && Array.isArray(offer.optional_features), "feature-negotiated API 0.4 offer required (not canonical contract)");
  const offered = [...offer.required_features, ...offer.optional_features];
  params(offered.every((feature) => typeof feature === "string") && new Set(offered).size === offered.length,
    "duplicate or invalid offered features");
  params(offer.required_features.every((feature) => SUPPORTED.has(feature)), "unsupported required protocol feature");
  params(REQUIRED.every((feature) => offered.includes(feature)), "host must offer tool_composition_v1, request_cancellation and content_parts");
  params(offer.required_features.includes("request_cancellation") && offer.required_features.includes("content_parts"), "foundation features must be required");
  params(exactKeys(offer.limits, ["max_concurrent_requests"]) && Number.isSafeInteger(offer.limits.max_concurrent_requests) &&
    offer.limits.max_concurrent_requests >= 4 && offer.limits.max_concurrent_requests <= 64, "host concurrency offer must be 4..64");
  params(Array.isArray(init.flag_values), "flag_values must be a list");
  const flags = { "codemode-mode": "on", "codemode-inline-budget": 3000 };
  const seen = new Set();
  for (const flag of init.flag_values) {
    params(exactKeys(flag, ["name", "value"]) && has(flag, "value") && has(flags, flag.name) && !seen.has(flag.name), "unknown/duplicate/malformed flag");
    seen.add(flag.name); flags[flag.name] = flag.value;
  }
  params(["on", "only"].includes(flags["codemode-mode"]), "codemode-mode must be on or only");
  params(Number.isSafeInteger(flags["codemode-inline-budget"]) && flags["codemode-inline-budget"] >= 0 &&
    flags["codemode-inline-budget"] <= 16000, "codemode-inline-budget must be an integer from 0 to 16000");
  const features = offered.filter((feature) => SUPPORTED.has(feature));
  const maxConcurrent = Math.min(8, offer.limits.max_concurrent_requests);
  return { features: new Set(features), flags, maxConcurrent, result: {
    api_version: "0.4", protocol: { version: "0.4", features, limits: { max_concurrent_requests: maxConcurrent } },
    tools: [{ name: "codemode", description: DESCRIPTION, parameters: CODE_SCHEMA,
      composition: { mode: flags["codemode-mode"], inline_budget: flags["codemode-inline-budget"] },
      constrained_sampling: { type: "grammar", variants: { openai_lark: CODEMODE_SOURCE_GRAMMAR } } }],
    commands: [{ name: "codemode", description: "Show codemode status, limits and JavaScript help", usage: "/codemode [status|help]" }],
  } };
}

/** A bounded, single serialized writer; no frame is split or abandoned. */
export class Writer {
  constructor(output, fatal) { this.output = output; this.fatal = fatal; this.queue = []; this.bytes = 0; this.busy = false; this.waiters = []; this.closed = false; }
  send(value, { shouldWrite = () => true, onStart = () => {} } = {}) {
    if (this.closed) return Promise.reject(new Error("Transport writer closed"));
    const line = JSON.stringify(value);
    if (Buffer.byteLength(line) > FRAME_BYTES) return Promise.reject(new RpcError(-32002, "Outgoing JSON-RPC frame exceeds 1 MiB"));
    const data = Buffer.from(`${line}\n`);
    if (this.queue.length >= 128 || this.bytes + data.length > 8 * FRAME_BYTES) {
      const error = new Error("Protocol writer queue exhausted (128 frames/8 MiB)"); this.fatal(error);
      return Promise.reject(error);
    }
    return new Promise((resolve, reject) => {
      this.queue.push({ data, resolve, reject, shouldWrite, onStart }); this.bytes += data.length; this.pump();
    });
  }
  pump() {
    if (this.busy) return;
    const item = this.queue.shift();
    if (!item) { for (const resolve of this.waiters.splice(0)) resolve(); return; }
    this.bytes -= item.data.length;
    if (!item.shouldWrite()) { item.resolve(); this.pump(); return; }
    this.busy = true; item.onStart();
    this.output.write(item.data, (error) => {
      this.busy = false;
      if (error) {
        this.closed = true; item.reject(error);
        for (const queued of this.queue.splice(0)) queued.reject(error);
        this.bytes = 0; this.fatal(error);
      } else item.resolve();
      this.pump();
    });
  }
  flush() { return !this.busy && !this.queue.length ? Promise.resolve() : new Promise((resolve) => this.waiters.push(resolve)); }
}

export class Runtime {
  constructor({ input = process.stdin, output = process.stdout, diagnostics = process.stderr, scratch = process.env.OCTET_EXTENSION_SCRATCH } = {}) {
    this.input = input; this.diagnostics = diagnostics; this.scratch = scratch;
    this.writer = new Writer(output, (error) => { void this.stop(true, error); });
    this.active = new Map(); this.pending = new Map(); this.seen = new Set(); this.counter = 0;
    this.state = "starting"; this.buffer = Buffer.alloc(0); this.maxConcurrent = 8; this.transportLost = false;
  }
  log(error) { this.diagnostics.write(`octet-codemode: ${head(messageOf(error).replace(/[\x00-\x1f\x7f]/g, " "), 2048)}\n`); }
  send(value, options) {
    return this.writer.send(value, options).catch((error) => { void this.stop(true, error); throw error; });
  }
  notify(method, params_) { if (!this.transportLost) void this.send({ jsonrpc: "2.0", method, params: params_ }).catch(() => {}); }
  progress(id, message) {
    const active = this.active.get(id);
    if (!active || !this.features?.has("request_progress") || active.controller.signal.aborted) return;
    this.notify("$/progress", { request_id: id, sequence: ++active.sequence, event: { type: "status", message } });
  }
  request(method, params_, signal) {
    cancelled(signal);
    if (this.state !== "ready" || this.transportLost) return Promise.reject(new RpcError(-32002, "Composition transport unavailable"));
    if (this.counter >= 65536 || this.pending.size >= 128) return Promise.reject(new RpcError(-32002, "Reverse request ID/pending limit exhausted; reload the extension"));
    const id = `codemode:${++this.counter}`;
    return new Promise((resolve, reject) => {
      const slot = { id, parent: params_.parent_request_id, sent: false, resolve, reject, signal };
      slot.finish = (error, result) => {
        if (!this.pending.delete(id)) return;
        signal?.removeEventListener("abort", slot.abort);
        if (error) reject(error); else resolve(result);
      };
      slot.abort = () => {
        slot.finish(new RpcError(-32800, "Request cancelled"));
        if (slot.sent) this.notify("$/cancelRequest", { id, reason: "cancelled" });
      };
      this.pending.set(id, slot); signal?.addEventListener("abort", slot.abort, { once: true });
      void this.send({ jsonrpc: "2.0", id, method, params: params_ },
        { shouldWrite: () => this.pending.has(id), onStart: () => { slot.sent = true; } }).catch((error) => slot.finish(error));
    });
  }
  response(message) {
    const slot = this.pending.get(message.id);
    if (!slot) {
      const match = typeof message.id === "string" && /^codemode:([1-9][0-9]*)$/.exec(message.id);
      if (!match || Number(match[1]) > this.counter) { void this.stop(true, new Error("Unknown reverse response ID")); return; }
      // Tombstoned late replies never reenter a guest or revive a cancelled
      // request. Sidecars still receive the same safe validation/cleanup.
      const file = message.result?.value_file ?? message.result?.context_file;
      if (file) void readHostJsonFile(file, this.scratch).catch((error) => this.log(error));
      return;
    }
    if (has(message, "error")) slot.finish(new RpcError(message.error.code, message.error.message));
    else slot.finish(undefined, message.result);
  }
  cancel(parameters) {
    if (!exactKeys(parameters, ["id", "reason"]) || !validId(parameters.id) ||
        (has(parameters, "reason") && (typeof parameters.reason !== "string" || Buffer.byteLength(parameters.reason) > 4096))) {
      this.log("Invalid cancellation notification"); return;
    }
    const active = this.active.get(parameters.id);
    if (active) active.controller.abort(new Error("Host cancelled parent request"));
    const reverse = this.pending.get(parameters.id);
    if (reverse) {
      reverse.finish(new RpcError(-32800, "Host cancelled reverse request"));
      // Loss of an operation-scoped waiter is not a successful script even if
      // the guest catches an Error. Never persist writes after this boundary.
      this.active.get(reverse.parent)?.controller.abort(new Error("Host cancelled outstanding reverse request"));
    }
  }
  error(id, code, message) {
    if (!this.transportLost) void this.send({ jsonrpc: "2.0", id, error: { code, message: head(message, 4096) } }).catch(() => {});
  }
  frame(bytes) {
    let message;
    try { message = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)); validateJson(message); }
    catch (error) { this.error(null, -32700, "Parse error"); this.log(error); return; }
    const id = isObject(message) && validId(message.id) ? message.id : null;
    if (!isObject(message) || message.jsonrpc !== "2.0") { this.error(id, -32600, "Invalid Request"); return; }
    if (!has(message, "method")) {
      if (!exactKeys(message, ["jsonrpc", "id", "result", "error"]) || id === null || has(message, "result") === has(message, "error") ||
          (has(message, "error") && (!exactKeys(message.error, ["code", "message", "data"]) || !Number.isInteger(message.error.code) || typeof message.error.message !== "string"))) {
        void this.stop(true, new Error("Invalid reverse response envelope")); return;
      }
      this.response(message); return;
    }
    if (!exactKeys(message, ["jsonrpc", "id", "method", "params"]) || typeof message.method !== "string" || !isObject(message.params)) {
      this.error(id, -32600, "Invalid Request"); return;
    }
    if (!has(message, "id")) {
      if (message.method === "$/cancelRequest" && this.features?.has("request_cancellation")) this.cancel(message.params);
      else this.log("Unknown/unnegotiated notification");
      return;
    }
    if (!Number.isSafeInteger(message.id) || message.id < 0) { this.error(id, -32600, "Host request ID must be an unsigned portable integer"); return; }
    if (message.method === "shutdown") {
      if (!exactKeys(message.params, [])) { this.error(id, -32602, "Invalid params: shutdown takes no fields"); return; }
      if (this.seen.has(id)) { this.error(id, -32600, "Duplicate host request ID"); return; }
      this.seen.add(id);
      void this.shutdown(id); return;
    }
    if (this.state === "draining" || this.state === "stopped") { this.error(id, -32002, "Extension is draining"); return; }
    if (this.seen.has(id) || this.seen.size >= 65536) { this.error(id, -32600, "Duplicate/exhausted host request ID"); return; }
    this.seen.add(id);
    if (this.active.size >= this.maxConcurrent) { this.error(id, -32002, "Negotiated host request concurrency exhausted"); return; }
    void this.dispatch(message);
  }
  async dispatch({ id, method, params: parameters }) {
    const controller = new AbortController();
    const slot = { controller, sequence: 0, method }; this.active.set(id, slot);
    slot.done = (async () => {
      let result;
      try {
        if (method === "initialize") {
          params(this.state === "starting", "initialize may run only once");
          const selected = negotiate(parameters);
          this.features = selected.features; this.flags = selected.flags; this.maxConcurrent = selected.maxConcurrent;
          this.executor = new Executor({ rpc: this, scratch: this.scratch, features: this.features, progress: (parent, message) => this.progress(parent, message) });
          this.state = "ready"; result = selected.result;
        } else {
          if (this.state !== "ready") throw new RpcError(-32002, "initialize is required before requests");
          if (method === "tool/call") {
            params(exactKeys(parameters, ["name", "arguments", "context"]) && parameters.name === "codemode" && isObject(parameters.context), "expected codemode tool/call with arguments and context");
            if (this.executor.running) this.progress(id, "Queued behind the active script");
            result = await this.executor.execute(id, parameters.arguments, controller.signal);
          } else if (method === "command/execute") {
            params(exactKeys(parameters, ["name", "arguments", "context"]) && parameters.name === "codemode" && isObject(parameters.context) &&
              Array.isArray(parameters.arguments) && parameters.arguments.length <= 1 && parameters.arguments.every((arg) => ["status", "help"].includes(arg)), "codemode command accepts status or help");
            result = { text: parameters.arguments[0] === "help" ? `${this.status()}\n\n${DESCRIPTION}\n\nExample: return await tools.read({path: \"README.md\"});` : this.status(), notifications: [], context: [] };
          } else if (method === "menu/collect") {
            params(exactKeys(parameters, ["context"]) && isObject(parameters.context), "menu context required");
            result = { title: "Codemode", status: { state: this.executor.running ? "running" : "active", label: this.executor.running ? "Script running" : "Ready" },
              detail: this.status(), items: [
                { id: "status", label: "Status and limits", command: "codemode", arguments: ["status"], recommended: true },
                { id: "help", label: "JavaScript help", command: "codemode", arguments: ["help"] },
              ] };
          } else throw new RpcError(-32601, "Method not found");
        }
        cancelled(controller.signal);
        if (!this.transportLost) await this.send({ jsonrpc: "2.0", id, result });
      } catch (error) {
        if (!this.transportLost) this.error(id, controller.signal.aborted ? -32800 : error.code instanceof Number ? Number(error.code) : Number.isInteger(error.code) ? error.code : -32603,
          controller.signal.aborted ? "Request cancelled" : messageOf(error));
      } finally { this.active.delete(id); }
    })();
    await slot.done;
  }
  status() {
    return `Pi codemode 1.0.0 / quickjs-wasi 3.6.2 · mode ${this.flags["codemode-mode"]} · inline budget ${this.flags["codemode-inline-budget"]}\n${this.executor.running ? "One script running" : "Ready"}; one active script, four concurrent nested calls, 256 calls, 256 MiB VM heap, <=25 s local / 30 s host deadline.\nOutput <=50 KiB; successful store writes are branch-scoped. Models helpers are unavailable. Change presentation with --codemode-mode on|only and --codemode-inline-budget 0..16000 at startup. No setup/npm install needed.`;
  }
  async shutdown(id) {
    this.state = "draining";
    for (const slot of this.active.values()) slot.controller.abort(new Error("Extension shutdown"));
    await Promise.all([...this.active.values()].map((slot) => slot.done));
    if (!this.transportLost) await this.send({ jsonrpc: "2.0", id, result: {} }).catch(() => {});
    await this.stop(false);
  }
  async stop(lost, reason) {
    if (this.stopping) return this.stopping;
    if (lost) { this.transportLost = true; if (reason) this.log(reason); }
    this.state = "draining";
    this.input.pause();
    this.stopping = (async () => {
      for (const slot of this.active.values()) slot.controller.abort(new Error("Extension transport closed"));
      for (const slot of [...this.pending.values()]) slot.finish(new RpcError(-32800, "Extension transport closed"));
      await Promise.all([...this.active.values()].map((slot) => slot.done));
      await this.writer.flush(); this.state = "stopped";
      this.resolveDone?.(reason ? 1 : 0);
    })();
    return this.stopping;
  }
  run() {
    return new Promise((resolve) => {
      this.resolveDone = resolve;
      this.input.on("data", (chunk) => {
        if (this.state === "stopped" || this.transportLost) return;
        const data = Buffer.concat([this.buffer, Buffer.from(chunk)]);
        let offset = 0;
        for (;;) {
          const newline = data.indexOf(10, offset);
          if (newline < 0) break;
          if (newline - offset > FRAME_BYTES) { void this.stop(true, new Error("Incoming JSON-RPC frame exceeds 1 MiB")); return; }
          this.frame(data.subarray(offset, newline)); offset = newline + 1;
        }
        this.buffer = data.subarray(offset);
        if (this.buffer.length > FRAME_BYTES) void this.stop(true, new Error("Incoming JSON-RPC frame exceeds 1 MiB"));
      });
      this.input.on("end", () => { void this.stop(true, this.buffer.length ? new Error("Truncated JSON-RPC frame at EOF") : undefined); });
      this.input.on("error", (error) => { void this.stop(true, error); });
      this.input.resume();
    });
  }
}

export async function main() {
  const [major, minor] = process.versions.node.split(".").map(Number);
  if (major < 22 || (major === 22 && minor < 19)) {
    process.stderr.write("octet-codemode requires Node.js >=22.19.0; install Node and reload (no npm install is needed).\n");
    return 1;
  }
  const runtime = new Runtime();
  const interrupted = () => { void runtime.stop(true); };
  process.once("SIGTERM", interrupted); process.once("SIGINT", interrupted);
  process.stdout.on("error", (error) => { void runtime.stop(true, error); });
  const code = await runtime.run();
  process.removeListener("SIGTERM", interrupted); process.removeListener("SIGINT", interrupted);
  return code;
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then((code) => process.exit(code), (error) => { process.stderr.write(`octet-codemode: ${head(messageOf(error), 2048)}\n`); process.exit(1); });
}

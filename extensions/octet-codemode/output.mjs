import { chmod, mkdir, mkdtemp, unlink, writeFile } from "node:fs/promises";
import { randomBytes, createHash } from "node:crypto";
import { isAbsolute, join, relative } from "node:path";
import { cancelled, exactKeys, head, tail, OUTPUT_BYTES, messageOf, preview } from "./common.mjs";

export class Scratch {
  constructor(root) { this.root = root; this.files = []; this.bytes = 0; }
  async directory() {
    if (!this.directoryPromise) this.directoryPromise = (async () => {
      if (typeof this.root !== "string" || !isAbsolute(this.root)) throw new Error("OCTET_EXTENSION_SCRATCH must be an absolute host-owned directory");
      await mkdir(this.root, { recursive: true, mode: 0o700 });
      const directory = await mkdtemp(join(this.root, "codemode-"));
      await chmod(directory, 0o700);
      return directory;
    })();
    return this.directoryPromise;
  }
  async write(data, suffix) {
    const path = join(await this.directory(), `${randomBytes(12).toString("hex")}.${suffix}`);
    await writeFile(path, data, { flag: "wx", mode: 0o600 });
    return path;
  }
  async spill(text) {
    const bytes = Buffer.byteLength(text);
    while (this.files.length >= 32 || this.bytes + bytes > 64 * 1024 * 1024) {
      const oldest = this.files.shift();
      if (!oldest) throw new Error("Full output exceeds private scratch retention budget");
      await unlink(oldest.path).catch((error) => { if (error.code !== "ENOENT") throw error; });
      this.bytes -= oldest.bytes;
    }
    const path = await this.write(text, "txt");
    this.files.push({ path, bytes }); this.bytes += bytes;
    return path;
  }
  async publish(item, parent, signal, rpc, features) {
    cancelled(signal);
    if (!features.has("artifacts")) throw new Error("image() requires the host's optional artifacts feature");
    if (!["image/png", "image/jpeg", "image/gif", "image/webp"].includes(item.mimeType)) throw new Error("Unsupported image MIME type");
    const data = Buffer.from(item.data, "base64");
    if (data.toString("base64") !== item.data || data.length === 0 || data.length > 20 * 1024 * 1024) throw new Error("Invalid or oversized image data");
    const params = { parent_request_id: parent, mime_type: item.mimeType, size: data.length,
      sha256: createHash("sha256").update(data).digest("hex") };
    let path;
    try {
      if (data.length <= 256 * 1024) params.data = { encoding: "base64", data: item.data };
      else { path = await this.write(data, "image"); params.path = relative(this.root, path).split("\\").join("/"); }
      cancelled(signal);
      const result = await rpc.request("artifact/publish", params, signal);
      if (!exactKeys(result, ["artifact_id"]) || typeof result.artifact_id !== "string" || !result.artifact_id || Buffer.byteLength(result.artifact_id) > 256) {
        throw new Error("Invalid artifact/publish response");
      }
      return { type: "image", artifact_id: result.artifact_id, mime_type: item.mimeType };
    } finally {
      if (path) await unlink(path).catch((error) => { if (error.code !== "ENOENT") throw error; });
    }
  }
}

export function callMetadata(calls, extra) {
  const metadata = { ...extra, call_count: calls.length, calls: [], calls_omitted: 0 };
  let used = Buffer.byteLength(JSON.stringify(metadata));
  for (const call of calls) {
    const row = { id: call.id, name: call.name, args: head(call.args, 200), status: call.status,
      duration_ms: Math.round(call.durationMs ?? 0), ...(call.error ? { error: head(call.error, 500) } : {}) };
    const size = Buffer.byteLength(JSON.stringify(row)) + 1;
    if (used + size > 60 * 1024) { metadata.calls_omitted++; continue; }
    metadata.calls.push(row); used += size;
  }
  return metadata;
}

export async function formatResult({ ok, items, error, calls, timeoutMs, maxCalls, maxTokens, wallMs, scratch }) {
  const body = items.filter((item) => item.type === "text").map((item) => item.text).join("\n");
  const errorText = error ? `\nScript error:\n${head(error.stack ?? error.message ?? messageOf(error), 4096)}\nTool calls made before failure are not undone.\n` : "";
  const header = `${ok ? "Script completed" : "Script failed"}\nWall time ${(wallMs / 1000).toFixed(1)} seconds\nOutput:\n`;
  // Header/error/truncation notices are outside the token approximation; the
  // complete model-visible text including those notices still fits 50 KiB.
  const bytes = Math.max(0, OUTPUT_BYTES - Buffer.byteLength(header + errorText) - 2048);
  const budget = Math.min(maxTokens * 4, bytes);
  const truncated = body.length > budget || Buffer.byteLength(body) > bytes;
  let visible = body;
  let fullOutputPath;
  if (truncated) {
    const count = Math.floor(budget / 2);
    const start = head(body.slice(0, count), Math.floor(bytes / 2));
    const end = budget - count > 0 ? tail(body.slice(-(budget - count)), Math.ceil(bytes / 2)) : "";
    visible = `Warning: truncated output (original estimated tokens: ${Math.ceil(body.length / 4)})\n${start}\n…output omitted…\n${end}`;
    try {
      fullOutputPath = await scratch.spill(body + errorText);
      visible += `\n[Full UTF-8 output: ${fullOutputPath} (read with offset/limit; oldest files expire)]`;
    } catch (error) { visible += `\n[Could not save full output: ${head(messageOf(error), 500)}]`; }
  }
  const text = header + head(visible, OUTPUT_BYTES - Buffer.byteLength(header + errorText)) + errorText;
  return {
    content: [{ type: "text", text }, ...items.filter((item) => item.type === "image")], is_error: !ok,
    metadata: callMetadata(calls, { timeout_ms: timeoutMs, max_calls: maxCalls, output_truncated: truncated,
      ...(fullOutputPath ? { full_output_path: fullOutputPath } : {}), ...(error ? { error_kind: error.kind ?? "sandbox" } : {}) }),
  };
}

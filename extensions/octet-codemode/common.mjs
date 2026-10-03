export const FRAME_BYTES = 1024 * 1024;
export const MAX_CALLS = 256;
export const LOCAL_TIMEOUT_MS = 25_000;
export const VM_HEAP_BYTES = 256 * 1024 * 1024;
export const OUTPUT_BYTES = 50 * 1024;
export const CAPTURE_BYTES = 16 * 1024 * 1024;
export const MAX_HOST_FILE_BYTES = 8 * 1024 * 1024;
export const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
export const has = (object, key) => Object.hasOwn(object, key);
export const exactKeys = (object, allowed) => isObject(object) && Object.keys(object).every((key) => allowed.includes(key));
export const validId = (id) => (Number.isSafeInteger(id) && id >= 0) || (typeof id === "string" && id.length > 0 && Buffer.byteLength(id) <= 256);

export class RpcError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}
export function cancelled(signal) {
  if (signal?.aborted) throw new RpcError(-32800, "Request cancelled");
}
export function params(condition, message) {
  if (!condition) throw new RpcError(-32602, `Invalid params: ${message}`);
}
export function messageOf(error) {
  return error instanceof Error ? error.message : String(error);
}
export function head(text, bytes) {
  if (Buffer.byteLength(text) <= bytes) return text;
  // Decode only complete UTF-8 code points (also never splits a surrogate pair).
  const buffer = Buffer.from(text);
  let end = Math.max(0, bytes);
  while (end > 0 && (buffer[end] & 0xc0) === 0x80) end--;
  return buffer.subarray(0, end).toString("utf8");
}
export function tail(text, bytes) {
  const buffer = Buffer.from(text);
  if (buffer.length <= bytes) return text;
  let start = buffer.length - Math.max(0, bytes);
  while (start < buffer.length && (buffer[start] & 0xc0) === 0x80) start++;
  return buffer.subarray(start).toString("utf8");
}
export function preview(value, bytes = 200) {
  const text = JSON.stringify(value) ?? "";
  return Buffer.byteLength(text) > bytes ? `${head(text, bytes - 3)}...` : text;
}
// The host owns schema validation of calls. This validates JSON boundaries, not
// tool schemas: no exotic numeric values, lone surrogates, or excessive depth.
export function validateJson(value, depth = 0) {
  if (depth > 32) throw new Error("JSON nesting exceeds 32 levels");
  if (typeof value === "number" && !Number.isFinite(value)) throw new Error("Non-finite JSON number");
  if (typeof value === "string" && /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)) {
    throw new Error("Malformed Unicode surrogate");
  }
  if (Array.isArray(value)) for (const child of value) validateJson(child, depth + 1);
  else if (isObject(value)) for (const [key, child] of Object.entries(value)) {
    validateJson(key, depth + 1);
    validateJson(child, depth + 1);
  }
}

import { constants } from "node:fs";
import { lstat, open, unlink } from "node:fs/promises";
import { createHash } from "node:crypto";
import { isAbsolute, join } from "node:path";
import { exactKeys, has, isObject, MAX_HOST_FILE_BYTES, validateJson } from "./common.mjs";

/** Only trusted Node sees this transport sidecar. It is never a guest global. */
export async function readHostJsonFile(reference, scratch) {
  if (!exactKeys(reference, ["path", "bytes", "sha256"]) ||
      typeof reference.path !== "string" || !/^[A-Za-z0-9_.-]{1,256}$/.test(reference.path) ||
      [".", ".."].includes(reference.path) || !Number.isSafeInteger(reference.bytes) ||
      reference.bytes <= 0 || reference.bytes > MAX_HOST_FILE_BYTES ||
      typeof reference.sha256 !== "string" || !/^[a-f0-9]{64}$/.test(reference.sha256) ||
      typeof scratch !== "string" || !isAbsolute(scratch)) {
    throw new Error("Invalid composition scratch-file reference (flat basename, exact size <=8 MiB and SHA256 required)");
  }
  const path = join(scratch, reference.path);
  let handle;
  try {
    const before = await lstat(path);
    if (!before.isFile() || before.isSymbolicLink()) throw new Error("Composition scratch file is not a regular non-symlink file");
    handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0));
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size !== reference.bytes || stat.dev !== before.dev || stat.ino !== before.ino) {
      throw new Error("Composition scratch-file size/type/identity mismatch");
    }
    // Read one extra byte so a concurrently grown file cannot pass or cause an
    // unbounded allocation even on a platform without O_NOFOLLOW.
    const buffer = Buffer.alloc(reference.bytes + 1);
    let length = 0;
    while (length < buffer.length) {
      const { bytesRead } = await handle.read(buffer, length, buffer.length - length, length);
      if (bytesRead === 0) break;
      length += bytesRead;
    }
    if (length !== reference.bytes || (await handle.stat()).size !== reference.bytes) {
      throw new Error("Composition scratch-file byte count mismatch");
    }
    const data = buffer.subarray(0, length);
    if (createHash("sha256").update(data).digest("hex") !== reference.sha256) {
      throw new Error("Composition scratch-file SHA256 mismatch");
    }
    const value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(data));
    validateJson(value);
    return value;
  } finally {
    try { await handle?.close(); } finally { await unlink(path).catch((error) => { if (error.code !== "ENOENT") throw error; }); }
  }
}

export async function compositionContext(response, scratch) {
  if (isObject(response) && has(response, "context_file")) {
    if (!exactKeys(response, ["context_file"])) throw new Error("Invalid composition/context sidecar envelope");
    return await readHostJsonFile(response.context_file, scratch);
  }
  return response;
}
export async function compositionValue(response, scratch) {
  if (isObject(response) && has(response, "value_file")) {
    if (!exactKeys(response, ["value_file"])) throw new Error("Invalid composition/call sidecar envelope");
    return await readHostJsonFile(response.value_file, scratch);
  }
  if (!exactKeys(response, ["value"]) || !has(response, "value")) throw new Error("Invalid composition/call response; expected {value}");
  return response.value;
}

// Test-only pipe binding to the compiled production Rust editor service.
// Never substitute a JS model or Pi Editor when the native peer is unavailable.
import { spawn } from 'node:child_process';
import { readSync, writeSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { rpcError } from '../lib/errors.mjs';

export function nativeEditorHost(t) {
  const binary = process.env.OCTET_NATIVE_EDITOR_TEST_HOST
    ?? (process.env.CARGO_TARGET_DIR && join(process.env.CARGO_TARGET_DIR, 'debug/examples/native-editor-test-host'));
  if (!binary || !existsSync(binary)) throw new Error('Native Editor tests require the built native-editor-test-host example (CARGO_TARGET_DIR or OCTET_NATIVE_EDITOR_TEST_HOST); no JS fallback');
  const child = spawn(binary, [], {stdio: ['pipe', 'pipe', 'inherit']});
  // No asynchronous reader owns these test-only pipes. Blocking native calls
  // intentionally exercise the facade's synchronous contract.
  child.stdin._handle.setBlocking(true);
  child.stdout._handle.readStop();
  child.stdout._handle.setBlocking(true);
  const input = child.stdin._handle.fd, output = child.stdout._handle.fd;
  let pending = Buffer.alloc(0), closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    child.stdin.destroy(); child.stdout.destroy(); child.kill();
  };
  t.after(close);
  function call(method, params) {
    if (closed) throw new Error('Native Editor fixture closed');
    const bytes = Buffer.from(JSON.stringify({method, params}) + '\n');
    for (let offset = 0; offset < bytes.length;) offset += writeSync(input, bytes, offset);
    let end;
    while ((end = pending.indexOf(10)) === -1) {
      const buffer = Buffer.alloc(65536), count = readSync(output, buffer);
      if (!count) throw new Error('Native Editor fixture EOF');
      pending = Buffer.concat([pending, buffer.subarray(0, count)]);
      if (pending.length > 1048576) throw new Error('Native Editor fixture reply exceeds frame bound');
    }
    const reply = JSON.parse(pending.subarray(0, end));
    pending = pending.subarray(end + 1);
    if (reply.error) throw rpcError(reply.error.code, reply.error.message);
    return reply.result;
  }
  return {call, close};
}

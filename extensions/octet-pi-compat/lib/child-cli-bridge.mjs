// Optional local-only launch bridge. It owns NO agent or transcript. A private
// Unix socket merely carries JSON observations to the facade CLI; every effect
// still goes through the captured host binding. Never install its env globally
// or reuse it across owner replacement: merge env for the admitted launch only.
import { createServer } from 'node:net';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { mkdtemp, chmod, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';
import { runChildCli } from './child-cli.mjs';
import { CHILD_FEATURES } from './children.mjs';
import { ownerKey, invalid, unsupported } from './errors.mjs';
const MAX_FRAME = 1048576, MAX_QUEUED = 4194304;
const timeout = globalThis.setTimeout, clear = globalThis.clearTimeout;

export async function createChildCliBridge(binding, { directory = tmpdir() } = {}) {
  ownerKey(binding?.owner); binding.assertLive();
  for (const feature of CHILD_FEATURES) if (!binding.features?.has(feature)) unsupported(feature);
  const folder = await mkdtemp(join(directory, 'octet-pi-child-'));
  await chmod(folder, 0o700);
  const socketPath = join(folder, 'host.sock');
  const token = randomBytes(32).toString('hex');
  const connections = new Set(), jobs = new Set();
  let closing = false;
  const server = createServer(socket => {
    if (closing || connections.size >= 8) { socket.destroy(); return; }
    connections.add(socket);
    let buffer = Buffer.alloc(0), started = false, terminal = false;
    const cancellation = new AbortController();
    const timer = timeout(() => socket.destroy(), 5000);
    const send = object => {
      if (socket.destroyed) throw new Error('child CLI observer disconnected');
      const line = JSON.stringify(object) + '\n';
      if (Buffer.byteLength(line) > MAX_FRAME || socket.writableLength + Buffer.byteLength(line) > MAX_QUEUED) throw new Error('child CLI observer backpressure exceeded');
      socket.write(line);
    };
    socket.on('error', () => {});
    socket.on('close', () => { clear(timer); connections.delete(socket); if (!terminal) cancellation.abort(new Error('child CLI disconnected')); });
    const fail = error => {
      cancellation.abort(error);
      try { send({ type: 'error', message: String(error?.message ?? error).slice(0, 2048), code: 1 }); } catch { /* Host cancellation still proceeds when observer is lost. */ }
      socket.end();
    };
    socket.on('data', chunk => {
      try {
        buffer = Buffer.concat([buffer, chunk]);
        if (buffer.length > MAX_FRAME) invalid('child CLI input frame exceeded');
        for (;;) {
          const at = buffer.indexOf(10); if (at < 0) break;
          const message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(buffer.subarray(0, at))); buffer = buffer.subarray(at + 1);
          if (!started) {
            const supplied = typeof message.token === 'string' ? Buffer.from(message.token) : Buffer.alloc(0);
            if (supplied.length !== 64 || !timingSafeEqual(supplied, Buffer.from(token))) { socket.destroy(); return; }
            if (Object.keys(message).some(key => !['token', 'argv'].includes(key))) invalid('unknown child CLI launch fields');
            binding.assertLive(); started = true; clear(timer);
            const job = runChildCli(message.argv, { binding, emit: event => send({ type: 'event', event }), signal: cancellation.signal });
            jobs.add(job);
            job.then(code => { terminal = true; try { send({ type: 'exit', code }); socket.end(); } catch { socket.destroy(); } }, fail).finally(() => jobs.delete(job));
          } else if (message.type === 'cancel' && Object.keys(message).length === 1) cancellation.abort(new Error('child CLI cancelled'));
          else invalid('child CLI accepts one launch and cancellation only');
        }
      } catch (error) { fail(error); }
    });
  });
  try { server.listen(socketPath); await once(server, 'listening'); await chmod(socketPath, 0o600); }
  catch (error) { server.close(); await rm(folder, { recursive: true, force: true }); throw error; }
  return {
    env: Object.freeze({ OCTET_PI_CHILD_SOCKET: socketPath, OCTET_PI_CHILD_TOKEN: token }),
    async close() {
      if (closing) return;
      closing = true;
      for (const socket of connections) socket.destroy();
      await new Promise(resolve => server.close(resolve));
      // Native deadlines/owner revocation remain authoritative if transport is
      // already lost. Do not hold adapter shutdown indefinitely for a provider.
      let timer;
      await Promise.race([Promise.allSettled([...jobs]), new Promise(resolve => { timer = timeout(resolve, 1500); })]);
      clear(timer); await rm(folder, { recursive: true, force: true });
    },
  };
}

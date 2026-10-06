import { connect } from 'node:net';
import { once } from 'node:events';
import { isAbsolute } from 'node:path';
import { parseChildArgv } from './child-cli.mjs';
import { invalid, unsupported } from './errors.mjs';

export async function runChildCliClient(argv, { env = process.env, output = process.stdout, signals = process } = {}) {
  parseChildArgv(argv); // Refuse unsupported effects before contacting the parent.
  const path = env.OCTET_PI_CHILD_SOCKET, token = env.OCTET_PI_CHILD_TOKEN;
  if (!path || !token) unsupported('Pi child CLI', 'an owner-bound octet launch bridge is required; upstream Pi is never started');
  if (!isAbsolute(path) || !/^[a-f0-9]{64}$/.test(token)) invalid('child CLI launch binding');
  const socket = connect(path);
  let timer, cancelled = false, terminal = false, buffer = Buffer.alloc(0);
  const cancel = () => {
    if (cancelled) return; cancelled = true;
    if (!socket.destroyed) socket.write(JSON.stringify({ type: 'cancel' }) + '\n');
    timer = setTimeout(() => socket.destroy(), 3000);
  };
  signals.on('SIGINT', cancel); signals.on('SIGTERM', cancel);
  try {
    await once(socket, 'connect');
    const request = JSON.stringify({ token, argv }) + '\n';
    if (Buffer.byteLength(request) > 262144) invalid('child CLI argv frame exceeded');
    socket.write(request);
    for await (const chunk of socket) {
      buffer = Buffer.concat([buffer, chunk]);
      for (;;) {
        const at = buffer.indexOf(10); if (at < 0) break;
        if (at > 1048576) invalid('child CLI response frame exceeded');
        const message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(buffer.subarray(0, at))); buffer = buffer.subarray(at + 1);
        if (terminal) invalid('child CLI emitted data after terminal response');
        if (message.type === 'event') {
          if (!output.write(JSON.stringify(message.event) + '\n')) await once(output, 'drain');
        } else if (message.type === 'exit') {
          if (![0, 130].includes(message.code)) invalid('child CLI exit disposition');
          terminal = true; return cancelled ? 130 : message.code;
        } else if (message.type === 'error') throw new Error(message.message);
        else invalid('child CLI response type');
      }
      if (buffer.length > 1048576) invalid('child CLI partial frame exceeded');
    }
    throw new Error('child CLI bridge closed without terminal settlement');
  } finally {
    clearTimeout(timer); signals.off('SIGINT', cancel); signals.off('SIGTERM', cancel); socket.destroy();
  }
}

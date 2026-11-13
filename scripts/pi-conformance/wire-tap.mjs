// Transparent tee between the host and the real runner: logs every JSON line in both directions.
// usage: node wire-tap.mjs <log-file> <command> [args...]
import { spawn } from 'node:child_process';
import { appendFileSync } from 'node:fs';
const [log, command, ...args] = process.argv.slice(2);
const child = spawn(command, args, { stdio: ['pipe', 'pipe', 'pipe'], env: process.env });
const tee = (direction, from, to) => {
  let buffer = '';
  from.on('data', chunk => {
    to.write(chunk); buffer += chunk;
    for (let i = buffer.indexOf('\n'); i >= 0; i = buffer.indexOf('\n')) { appendFileSync(log, `${direction} ${Date.now() % 1000000} ${buffer.slice(0, i)}\n`); buffer = buffer.slice(i + 1); }
  });
  from.on('end', () => to.end());
};
child.stderr.on('data', chunk => { process.stderr.write(chunk); appendFileSync(log, `ERR ${JSON.stringify(String(chunk))}\n`); });
child.on('exit', (code, signal) => appendFileSync(log, `EXIT ${code} ${signal}\n`));
tee('H>E', process.stdin, child.stdin);
tee('E>H', child.stdout, process.stdout);
child.on('exit', code => process.exit(code ?? 1));

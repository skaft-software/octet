// Replay raw PTY bytes through a real VT emulator and print the visible screen.
// usage: node render.mjs <raw-file> <columns> <rows>
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
// @xterm/headless is resolved from the working directory (E2E_PI_ROOT), where the harness installs it.
const xterm = createRequire(process.cwd() + '/')('@xterm/headless');
const [file, columns, rows] = [process.argv[2], Number(process.argv[3]), Number(process.argv[4])];
const term = new xterm.Terminal({ cols: columns, rows, allowProposedApi: true, scrollback: 0 });
await new Promise(resolve => term.write(readFileSync(file), resolve));
const buffer = term.buffer.active;
const lines = [];
for (let y = 0; y < rows; y++) lines.push(buffer.getLine(y)?.translateToString(true) ?? '');
process.stdout.write(lines.join('\n') + '\n');

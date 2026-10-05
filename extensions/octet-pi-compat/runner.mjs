import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { isolateStdout, Transport } from './lib/transport.mjs';
import { Runtime } from './lib/runtime.mjs';
import { fallbackData } from './lib/installed-pi.mjs';

const writer = isolateStdout();
try {
  const args = process.argv.slice(2);
  let inspect = false, piRuntime, configPath = fileURLToPath(new URL('./bridge.json', import.meta.url)), extensions = [];
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--config') configPath = resolve(args[++i]);
    else if (args[i] === '--inspect') inspect = true;
    else if (args[i] === '--pi-runtime') piRuntime = args[++i] ?? '';
    else if (args[i].startsWith('-')) throw new Error(`unknown runner option ${args[i]}`);
    else extensions.push(resolve(args[i]));
  }
  const config = extensions.length ? { extensions } : JSON.parse(readFileSync(configPath, 'utf8'));
  if (piRuntime !== undefined) config.pi_runtime = piRuntime; // explicit opt-in; validated by the runtime
  let runtime;
  const transport = new Transport(writer, { onMessage: m => runtime.receive(m), onLost: (e, eof) => runtime.lost(e, eof) });
  runtime = new Runtime(config, transport);
  runtime.timers.install();
  writer.setIntentHandler?.(enabled => runtime.mouseIntent(enabled));
  process.on('uncaughtException', error => transport.fail(error));
  process.on('unhandledRejection', error => transport.fail(error instanceof Error ? error : new Error(String(error))));
  if (inspect) {
    // Explicit review-only subprocess: no host, agent, terminal, provider, or
    // implicit discovery. Configure bounds its runtime and output.
    await runtime.load();
    await transport.send({ jsonrpc: '2.0', id: 0, result: runtime.metadata() });
    runtime.timers.all(); await transport.idle(); process.exit(0);
  } else transport.start();
} catch (error) {
  // One console write: the isolated console is a stream and exit drops queued writes.
  const fallback = fallbackData(error);
  console.error(`[pi-compat startup] ${error.message}${fallback ? `\n[pi-compat fallback] ${JSON.stringify(fallback)}` : ''}`);
  process.exit(1);
}

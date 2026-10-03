#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, realpathSync, writeFileSync, existsSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

// Importing a factory executes arbitrary user code. Never perform implicit
// discovery or change host trust/enablement. The caller must explicitly review
// the supplied factories and request this local registration capture.
const root = fileURLToPath(new URL('.', import.meta.url));
export function configure({ output, extensions, reviewed, overwrite = false }) {
  if (!reviewed) throw new Error('Factory execution requires --reviewed (review every entrypoint and its imports first)');
  output = resolve(output);
  if (basename(output) !== 'octet-pi-compat') throw new Error('--output direct-child directory must be named octet-pi-compat');
  if (!extensions.length || extensions.length > 64) throw new Error('Supply 1..64 explicitly reviewed entrypoints');
  extensions = extensions.map(p => realpathSync(resolve(p)));
  if (new Set(extensions).size !== extensions.length) throw new Error('Duplicate entrypoint');
  for (const name of ['extension.toml', 'bridge.json']) if (!overwrite && existsSync(join(output, name))) throw new Error(`${name} exists; use --overwrite after reviewing the changed catalog`);
  const runner = join(root, 'runner.mjs');
  const captured = spawnSync(process.execPath, [runner, '--inspect', ...extensions], { cwd: process.cwd(), encoding: 'utf8', timeout: 15000, maxBuffer: 1048576 });
  if (captured.error || captured.status !== 0) throw new Error(`registration capture failed: ${captured.error?.message || captured.stderr.slice(-4096)}`);
  const frames = captured.stdout.trim().split('\n').map(line => JSON.parse(line));
  if (frames.length !== 1 || !frames[0].result) throw new Error('registration capture must return one bounded RPC metadata frame');
  const registrations = frames[0].result;
  const entrypoint_sha256 = Object.fromEntries(extensions.map(entry => [entry, createHash('sha256').update(readFileSync(entry)).digest('hex')]));
  const config = { extensions, entrypoint_sha256, registrations };
  const quoted = value => JSON.stringify(value);
  const list = values => `[${values.map(quoted).join(', ')}]`;
  const flags = registrations.flags.map(flag => `{ name = ${quoted(flag.name)}, type = ${quoted(flag.type)}, default = ${quoted(flag.default)}${flag.description ? `, description = ${quoted(flag.description)}` : ''} }`);
  const manifest = [
    'name = "octet-pi-compat"', 'version = "0.1.0"', 'api_version = "0.4"',
    'description = "Optional reviewed Pi factories; Rust owns the agent and terminal"', '',
    '[entrypoint]', `command = ${quoted(realpathSync(process.execPath))}`,
    `args = ${list([runner, '--config', join(output, 'bridge.json')])}`, '',
    '# Trusted factories retain normal OS authority. Declarations are consent metadata, not a sandbox.',
    '[capabilities]', 'filesystem = "unrestricted"', 'process = true', 'network = true', '',
    '[contributes]', `tools = ${list(registrations.tools.map(t => t.name))}`,
    `commands = ${list(registrations.commands.map(c => c.name))}`, `hooks = ${list(registrations.hooks)}`,
    `tool_renderers = ${list(registrations.tool_renderers)}`,
    'notifications = true', 'confirmations = true',
    ...(flags.length ? [`flags = [\n  ${flags.join(',\n  ')},\n]`] : []), '',
  ].join('\n');
  mkdirSync(output, { recursive: true, mode: 0o700 });
  writeFileSync(join(output, 'bridge.json'), JSON.stringify(config, null, 2) + '\n', { mode: 0o600 });
  writeFileSync(join(output, 'extension.toml'), manifest, { mode: 0o600 });
  return { output, registrations };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2), extensions = []; let output, reviewed = false, overwrite = false;
    for (let i = 0; i < args.length; i++) {
      if (args[i] === '--output') output = args[++i];
      else if (args[i] === '--reviewed') reviewed = true;
      else if (args[i] === '--overwrite') overwrite = true;
      else if (args[i].startsWith('-')) throw new Error(`unknown configure option ${args[i]}`);
      else extensions.push(args[i]);
    }
    if (!output) throw new Error('Usage: node configure.mjs --reviewed --output /absolute/extensions/octet-pi-compat <entry.ts>...');
    const result = configure({ output, extensions, reviewed, overwrite });
    console.log(`Configured ${result.registrations.commands.length} commands, ${result.registrations.tools.length} tools in ${result.output}. Not enabled or trusted. Dependencies remain in ${root}.`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}

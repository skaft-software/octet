#!/usr/bin/env node
import { resolve, basename, dirname, join } from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { writeFileSync, existsSync } from 'node:fs';
import { diagnosticConsole } from './diagnostics.mjs';
import { Extension } from './index.mjs';

const usage = 'Usage: octet-extension run SOURCE | manifest SOURCE --name NAME --version VERSION --out extension.toml [--filesystem none|workspace|unrestricted] [--process] [--network]';
try {
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || major === 22 && minor < 19) throw new Error('Node >=22.19.0 is required');
  const [action, source, ...args] = process.argv.slice(2);
  if (!['run', 'manifest'].includes(action) || !source) throw new Error(usage);
  const options = {};
  if (action === 'run' && args.length) throw new Error(usage);
  for (let i = 0; i < args.length; i++) {
    const key = args[i];
    if (!['--name', '--version', '--out', '--filesystem', '--process', '--network'].includes(key) || Object.hasOwn(options, key)) throw new Error(usage);
    if (['--process', '--network'].includes(key)) options[key] = true;
    else {
      if (!args[i + 1] || args[i + 1].startsWith('--')) throw new Error(usage);
      options[key] = args[++i];
    }
  }
  const authorPath = resolve(source);
  // Module loading executes registration code, never handlers. Reserve stdout even here.
  const stdout = process.stdout.write;
  const originalConsole = globalThis.console;
  globalThis.console = diagnosticConsole();
  process.stdout.write = () => { throw new Error('stdout is reserved for the extension runtime'); };
  let extension;
  try { extension = (await import(pathToFileURL(authorPath).href)).default; }
  finally {
    process.stdout.write = stdout;
    globalThis.console = originalConsole;
  }
  if (!(extension instanceof Extension)) throw new Error('Author module must export default an Extension; do not call run() inside it');
  if (action === 'run') extension.run();
  else {
    const name = options['--name'];
    const version = options['--version'];
    const output = options['--out'];
    const filesystem = options['--filesystem'] ?? 'none';
    if (!name || !/^[a-z][a-z0-9-]{0,63}$/.test(name) || !version ||
        !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version) || !output ||
        basename(dirname(resolve(output))) !== name || !['none', 'workspace', 'unrestricted'].includes(filesystem)) throw new Error(usage);
    const {tools, commands} = extension.contributions();
    // JSON strings/arrays are valid TOML basic strings/arrays here; reject control input.
    const quote = value => {
      if (/[\u0000-\u001f\u007f]/.test(value)) throw new Error('Manifest strings must not contain controls');
      return JSON.stringify(value);
    };
    const array = values => `[${values.map(quote).join(', ')}]`;
    if (process.platform === 'win32') throw new Error('Local manifest launch is currently qualified only on Unix; Windows launcher generation is unsupported');
    const launcher = join(dirname(resolve(output)), '.octet-launcher.sh');
    if (existsSync(resolve(output)) || existsSync(launcher)) throw new Error('Refusing to overwrite an existing manifest or local launcher');
    const shellQuote = value => `'${value.replaceAll("'", "'\\''")}'`;
    const launch = `#!/bin/sh\n# Invoke the installed interpreter in place: the host stages this script, not Node.\nexec ${[process.execPath, fileURLToPath(import.meta.url), 'run', authorPath].map(shellQuote).join(' ')}\n`;
    // Fail closed on existing local files; don't overwrite a reviewed launcher.
    writeFileSync(launcher, launch, {flag: 'wx', mode: 0o700});
    const text = `# Generated local manifest. Regenerate after moving this package or Node.\nname = ${quote(name)}\nversion = ${quote(version)}\napi_version = "0.4"\n\n[entrypoint]\ncommand = ${quote(launcher)}\nargs = []\n\n[capabilities]\nfilesystem = ${quote(filesystem)}\nprocess = ${!!options['--process']}\nnetwork = ${!!options['--network']}\n\n[contributes]\ntools = ${array(tools)}\ncommands = ${array(commands)}\n`;
    writeFileSync(resolve(output), text, {flag: 'wx', mode: 0o600});
    console.error(`Generated ${resolve(output)} (local source; not a distributable bundle)`);
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : 'Extension authoring command failed');
  process.exitCode = 1;
}

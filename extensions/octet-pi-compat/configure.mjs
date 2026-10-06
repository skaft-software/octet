#!/usr/bin/env node
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { basename, join, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { hookEvents } from './lib/api.mjs';
import { BUILTIN_TOOL_NAMES } from './lib/tools.mjs';
import { locateInstalledPi } from './lib/installed-pi.mjs';
import { PI_VERSION } from './lib/pi-modules.mjs';
import { checkThemeImportOutput, planThemeImport, themeImportLaunchHint, writeThemeImport } from './lib/theme-import.mjs';

// Importing a factory executes arbitrary user code. Never perform implicit
// discovery or change host trust/enablement. The caller must explicitly review
// the supplied factories and request this local registration capture.
const root = fileURLToPath(new URL('.', import.meta.url));
// Runs the review-only registration capture for explicit entrypoints, each on
// its recorded route ('shims' = octet's emulated path, 'installed' = the
// installed Pi). Bounded by time and output; never touches host state.
function captureConfig(dir, extensions, routes, { piAgentDir, piTheme }) {
  const config = join(dir, 'bridge.json');
  // Probes/capture see the exact planned colors before publication. A failed
  // capture must not overwrite an existing import's palette files.
  const themePath = piTheme?.json === undefined ? piTheme?.path : join(dir, 'theme.json');
  if (piTheme?.json !== undefined) writeFileSync(themePath, piTheme.json, { mode: 0o600 });
  writeFileSync(config, JSON.stringify({ extensions, extension_runtimes: routes,
    ...(piAgentDir ? { pi_agent_dir: piAgentDir } : {}),
    ...(piTheme ? { pi_theme: { name: piTheme.name, path: themePath } } : {}) }));
  return config;
}
function capture(extensions, routes = {}, { cwd = process.cwd(), env = process.env, piAgentDir, piTheme } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'octet-pi-capture-'));
  try {
    const config = captureConfig(dir, extensions, routes, { piAgentDir, piTheme });
    return spawnSync(process.execPath, [join(root, 'runner.mjs'), '--config', config, '--inspect'],
      { cwd, env, encoding: 'utf8', timeout: 15000 + 2000 * extensions.length, maxBuffer: 4194304 });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
export function configure({ output, extensions, reviewed, overwrite = false, routes = {}, cwd = process.cwd(), env = process.env, piAgentDir, piTheme, themePaths = [] }) {
  if (!reviewed) throw new Error('Factory execution requires --reviewed (review every entrypoint and its imports first)');
  output = resolve(output);
  if (basename(output) !== 'octet-pi-compat') throw new Error('--output direct-child directory must be named octet-pi-compat');
  if (!extensions.length || extensions.length > 64) throw new Error('Supply 1..64 explicitly reviewed entrypoints');
  extensions = extensions.map(p => realpathSync(resolve(p)));
  if (new Set(extensions).size !== extensions.length) throw new Error('Duplicate entrypoint');
  for (const name of ['extension.toml', 'bridge.json']) if (!overwrite && existsSync(join(output, name))) throw new Error(`${name} exists; use --overwrite after reviewing the changed catalog`);
  const runner = join(root, 'runner.mjs');
  routes = Object.fromEntries(Object.entries(routes).map(([entry, route]) => [realpathSync(entry), route]));
  const captured = capture(extensions, routes, { cwd, env, piAgentDir, piTheme });
  if (captured.error || captured.status !== 0) throw new Error(`registration capture failed: ${captured.error?.message || captured.stderr.slice(-4096)}`);
  const skippedAtCapture = captured.stderr.split('\n').filter(line => line.startsWith('[pi-compat] skipped '));
  if (skippedAtCapture.length) throw new Error(`registration capture skipped extensions:\n${skippedAtCapture.join('\n').slice(0, 4096)}`);
  const frames = captured.stdout.trim().split('\n').map(line => JSON.parse(line));
  if (frames.length !== 1 || !frames[0].result) throw new Error('registration capture must return one bounded RPC metadata frame');
  const registrations = frames[0].result;
  const builtinOverrides = [...new Set(registrations.tools.map(tool => tool.name)
    .filter(name => builtinTools.has(name)))].sort();
  // Reviewed factories may subscribe later. Reserve the real mapped hook
  // channels up front; callbacks remain local and initially inert. Provider
  // wire hooks are an exception: while subscribed, the host refuses
  // extension-registered (host stream transport) providers. Resource discovery
  // also requires a complete native consumer. Reserve it for imported themes
  // or captured callbacks; late factory subscriptions must reconfigure.
  const captureOnly = new Set(['before_provider_request', 'before_provider_headers', 'after_provider_response', 'resources_discover']);
  const subscribed_hooks = [...new Set(Object.values(hookEvents))]
    .filter(hook => !captureOnly.has(hook) || registrations.hooks.includes(hook) || hook === 'resources_discover' && themePaths.length).sort();
  registrations.hooks = subscribed_hooks;
  const entrypoint_sha256 = Object.fromEntries(extensions.map(entry => [entry, createHash('sha256').update(readFileSync(entry)).digest('hex')]));
  const config = { extensions, entrypoint_sha256, registrations, subscribed_hooks,
    ...(Object.keys(routes).length ? { extension_runtimes: routes } : {}),
    ...(piAgentDir ? { pi_agent_dir: piAgentDir } : {}),
    ...(piTheme ? { pi_theme: { name: piTheme.name, path: piTheme.path, native_name: piTheme.selector, native_path: piTheme.nativePath } } : {}),
    ...(themePaths.length ? { pi_theme_paths: themePaths } : {}) };
  const quoted = value => JSON.stringify(value);
  const list = values => `[${values.map(quoted).join(', ')}]`;
  const shortcuts = registrations.shortcuts.map(shortcut => `{ name = ${quoted(shortcut.name)}, key = ${quoted(shortcut.key)}, description = ${quoted(shortcut.description)} }`);
  const flags = registrations.flags.map(flag => `{ name = ${quoted(flag.name)}, type = ${quoted(flag.type)}, default = ${quoted(flag.default)}${flag.description ? `, description = ${quoted(flag.description)}` : ''} }`);
  const manifest = [
    'name = "octet-pi-compat"', 'version = "0.1.0"', 'api_version = "0.4"',
    'description = "Optional reviewed Pi factories; Rust owns the agent and terminal"', '',
    '[entrypoint]', `command = ${quoted(realpathSync(process.execPath))}`,
    `args = ${list([runner, '--config', join(output, 'bridge.json')])}`, '',
    '# Trusted factories retain normal OS authority. Declarations are consent metadata, not a sandbox.',
    '[capabilities]', 'filesystem = "unrestricted"', 'process = true', 'network = true',
    'system_prompt = true',
    ...(builtinOverrides.length ? [`builtin_tool_overrides = ${list(builtinOverrides)}`] : []), '',
    '[contributes]', `tools = ${list(registrations.tools.map(t => t.name))}`,
    `commands = ${list(registrations.commands.map(c => c.name))}`, `hooks = ${list(registrations.hooks)}`,
    `tool_renderers = ${list(registrations.tool_renderers)}`,
    ...(shortcuts.length ? [`shortcuts = [\n  ${shortcuts.join(',\n  ')},\n]`] : []),
    'notifications = true', 'confirmations = true', 'providers = true',
    ...(flags.length ? [`flags = [\n  ${flags.join(',\n  ')},\n]`] : []), '',
  ].join('\n');
  mkdirSync(output, { recursive: true, mode: 0o700 });
  writeFileSync(join(output, 'bridge.json'), JSON.stringify(config, null, 2) + '\n', { mode: 0o600 });
  writeFileSync(join(output, 'extension.toml'), manifest, { mode: 0o600 });
  return { output, registrations };
}
// The user's enabled Pi extensions, resolved by the installed Pi 1.0.2's own
// package manager exactly as Pi would load them. Read-only: settings are
// never written and missing packages are skipped, never installed.
async function piResources({ cwd = process.cwd(), env = process.env } = {}) {
  const install = locateInstalledPi(env);
  const pi = await import(pathToFileURL(install.packages['coding-agent'].entry).href);
  const settingsManager = pi.SettingsManager.create(cwd, install.agentDir);
  const resolved = await new pi.DefaultPackageManager({ cwd, agentDir: install.agentDir, settingsManager }).resolve(async () => 'skip');
  return { install, resolved, settingsManager };
}
const enabledPaths = resources => resources.filter(r => r.enabled && !r.path.startsWith('builtin:')).map(r => r.path);
export async function piExtensions(options = {}) {
  return enabledPaths((await piResources(options)).resolved.extensions);
}

const builtinTools = new Set(BUILTIN_TOOL_NAMES);
// First-party octet extensions keep their tool names when a Pi extension
// registers the same one; test/import.test.mjs pins this to ../release-catalog.txt.
export const FIRST_PARTY_EXTENSIONS = Object.freeze(['octet-codemode', 'octet-computer-use', 'octet-mcp', 'octet-subagents', 'octet-web-search']);
// Installed first-party extensions live next to this adapter (bundles, source
// checkouts) or in the user's octet extension directory.
const defaultFirstPartyRoots = env => [resolve(root, '..'), join(env.HOME || homedir(), '.octet', 'extensions')];
// Maps each tool an installed first-party extension declares to its owner.
// Reads at most one bounded manifest per first-party name and root.
export function firstPartyTools(roots) {
  const owners = new Map();
  for (const dir of roots) for (const name of FIRST_PARTY_EXTENSIONS) {
    let source;
    try { source = readFileSync(join(dir, name, 'extension.toml'), 'utf8'); } catch { continue; }
    if (source.length > 1048576) continue;
    const section = source.split(/^\[contributes\]\s*$/m)[1]?.split(/^\[/m)[0] ?? '';
    const tools = section.match(/^tools\s*=\s*\[([^\]]*)\]/m)?.[1] ?? '';
    for (const [, tool] of tools.matchAll(/"([^"]+)"/g)) if (!owners.has(tool)) owners.set(tool, name);
  }
  return owners;
}
async function probe(entry, route, { cwd, env, owners = new Map(), piTheme }) {
  const dir = piTheme ? mkdtempSync(join(tmpdir(), 'octet-pi-probe-')) : undefined;
  try {
    const args = [join(root, 'runner.mjs'), '--pi-runtime', route, '--inspect',
      ...(dir ? ['--config', captureConfig(dir, [entry], {}, { piTheme })] : [entry])];
    return await new Promise(done => {
      const child = spawn(process.execPath, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
      let stderr = '', stdout = '', overflow = false;
      const timer = setTimeout(() => child.kill('SIGKILL'), 30000);
      child.stdout.on('data', chunk => {
        if (overflow) return;
        stdout += chunk;
        if (Buffer.byteLength(stdout) > 4194304) { overflow = true; child.kill('SIGKILL'); }
      });
      child.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-65536); });
      child.on('error', error => { clearTimeout(timer); done({ ok: false, error: error.message }); });
      child.on('close', code => {
        clearTimeout(timer);
        if (code !== 0 || overflow) return done({ ok: false, error: overflow ? 'registration output exceeds limit' : stderr.trim().split('\n')[0]?.replace(/^\[pi-compat startup\] /, '') || `exit ${code}` });
        try {
          const metadata = JSON.parse(stdout).result;
          const owned = metadata.tools.filter(tool => owners.has(tool.name)).map(tool => `${tool.name} is provided by the first-party octet extension ${owners.get(tool.name)}`);
          done(owned.length ? { ok: false, conflict: true, error: `turned off: ${owned.join('; ')}` } : { ok: true });
        } catch (error) { done({ ok: false, error: `invalid registration output: ${error.message}` }); }
      });
    });
  } finally { if (dir) rmSync(dir, { recursive: true, force: true }); }
}

// Chooses each extension's route: octet's emulated path when it loads there,
// otherwise the installed Pi; an extension that loads on neither is skipped.
export async function routeExtensions(extensions, { installed = true, concurrency = 6, cwd = process.cwd(), env = process.env, piTheme, firstPartyRoots = defaultFirstPartyRoots(env) } = {}) {
  if (!Array.isArray(extensions) || extensions.length > 64) throw new Error('Supply at most 64 entrypoints');
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 64) throw new Error('concurrency must be 1..64');
  const results = new Array(extensions.length), owners = firstPartyTools(firstPartyRoots);
  let next = 0;
  const worker = async () => {
    while (next < extensions.length) {
      const index = next++, entry = extensions[index];
      const emulated = await probe(entry, 'shims', { cwd, env, owners, piTheme });
      if (emulated.ok) { results[index] = { entry, route: 'shims' }; continue; }
      const viaPi = installed && !emulated.conflict ? await probe(entry, 'installed', { cwd, env, owners, piTheme }) : emulated;
      results[index] = viaPi.ok ? { entry, route: 'installed' } : { entry, route: null, error: viaPi.error };
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, extensions.length) }, worker));
  return results;
}

const label = entry => {
  const parts = entry.split(sep), at = parts.lastIndexOf('node_modules');
  if (at < 0) return entry;
  const name = parts[at + 1]?.startsWith('@') ? parts.slice(at + 1, at + 3).join('/') : parts[at + 1];
  const rest = parts.slice(at + (name.startsWith('@') ? 3 : 2)).join('/');
  return `${name} ${rest}`;
};

// One step from an installed Pi 1.0.2 setup to a configured bridge.
export async function configureFromPi({ output, reviewed, overwrite = false, cwd = process.cwd(), env = process.env, log = console.log }) {
  if (!reviewed) throw new Error('Loading your Pi extensions runs their code with your permissions, as in Pi; pass --reviewed to accept');
  output = resolve(output);
  if (basename(output) !== 'octet-pi-compat') throw new Error('--output direct-child directory must be named octet-pi-compat');
  for (const name of ['extension.toml', 'bridge.json']) if (!overwrite && existsSync(join(output, name))) throw new Error(`${name} exists; use --overwrite after reviewing the changed catalog`);
  checkThemeImportOutput(output, overwrite);
  const { install, resolved, settingsManager } = await piResources({ cwd, env });
  const piAgentDir = install.agentDir;
  const extensions = [...new Set(enabledPaths(resolved.extensions).map(entry => realpathSync(entry)))];
  if (!extensions.length) throw new Error(`no enabled extensions in your Pi ${PI_VERSION} setup`);
  const themeImport = planThemeImport({ output, paths: enabledPaths(resolved.themes), selection: settingsManager.getThemeSetting(),
    thinkingLevel: settingsManager.getDefaultThinkingLevel(), builtinDir: join(install.packages['coding-agent'].dir, 'dist/modes/interactive/theme') });
  checkThemeImportOutput(output, overwrite, themeImport.themes);
  const options = { cwd, env: { ...env, OCTET_PI_AGENT_DIR: piAgentDir }, piAgentDir, piTheme: themeImport.selected };
  const routed = await routeExtensions(extensions, options);
  let accepted = routed.filter(r => r.route);
  // Extensions that load alone can still collide (a duplicate command name);
  // drop the later one and capture again, as Pi keeps the first registration.
  while (accepted.length) {
    const routes = Object.fromEntries(accepted.map(r => [r.entry, r.route]));
    const captured = capture(accepted.map(r => r.entry), routes, options);
    const skippedLines = captured.stderr.split('\n').filter(line => line.startsWith('[pi-compat] skipped '));
    if (captured.status === 0 && !skippedLines.length) break;
    const skipped = new Set(skippedLines.map(line => line.slice('[pi-compat] skipped '.length).split(': ')[0]));
    for (const r of accepted) if (skipped.has(realpathSync(r.entry))) { r.route = null; r.error = skippedLines.find(l => l.includes(r.entry))?.split(': ').slice(1).join(': ') ?? 'skipped'; }
    if (!skipped.size) throw new Error(`registration capture failed: ${captured.stderr.slice(-4096)}`);
    accepted = accepted.filter(r => r.route);
  }
  if (!accepted.length) throw new Error('none of your Pi extensions loaded');
  const result = configure({ output, reviewed, overwrite, ...options, extensions: accepted.map(r => r.entry),
    piTheme: themeImport.selected, themePaths: themeImport.themes.map(theme => theme.nativePath),
    routes: Object.fromEntries(accepted.map(r => [r.entry, r.route])) });
  writeThemeImport(themeImport, { overwrite });
  for (const r of routed) log(r.route === 'shims' ? `  octet         ${label(r.entry)}` : r.route === 'installed' ? `  via Pi ${PI_VERSION}  ${label(r.entry)}` : `  skipped       ${label(r.entry)}: ${String(r.error).slice(0, 160)}`);
  for (const message of themeImport.diagnostics) log(`  ${message}`);
  log(`  Imported ${themeImport.themes.length} Pi themes; the enabled bridge contributes them and its startup preference. Host settings were not changed.`);
  if (themeImport.launchArgs.length) log(`  Optional standalone theme options (not needed with the bridge): ${themeImportLaunchHint(themeImport)}`);
  return { ...result, routed, themeImport };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2), extensions = []; let output, reviewed = false, overwrite = false, fromPi = false;
    for (let i = 0; i < args.length; i++) {
      if (args[i] === '--output') output = args[++i];
      else if (args[i] === '--reviewed') reviewed = true;
      else if (args[i] === '--overwrite') overwrite = true;
      else if (args[i] === '--from-pi') fromPi = true;
      else if (args[i].startsWith('-')) throw new Error(`unknown configure option ${args[i]}`);
      else extensions.push(args[i]);
    }
    if (!output) throw new Error('Usage: node configure.mjs --reviewed --output /absolute/extensions/octet-pi-compat (--from-pi | <entry.ts>...)');
    if (fromPi && extensions.length) throw new Error('--from-pi reads your Pi setup; do not also list entrypoints');
    const result = fromPi ? await configureFromPi({ output, reviewed, overwrite }) : configure({ output, extensions, reviewed, overwrite });
    console.log(`Configured ${result.registrations.commands.length} commands, ${result.registrations.tools.length} tools in ${result.output}. Not enabled or trusted. Dependencies remain in ${root}.`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}

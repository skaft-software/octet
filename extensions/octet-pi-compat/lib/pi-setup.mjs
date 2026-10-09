// Read-only mirror discovery for an existing Pi 1.0.2 setup.
//
// Mirror mode is an explicit, reviewed opt-in recorded by
// `configure.mjs --reviewed --from-pi --mirror`: the user asks Octet to mirror
// the Pi setup on this machine. This module only *reads* the Pi layout Pi 1.0.2
// itself reads (settings.json, keybindings.json, AGENTS.md and the user/project
// resource directories); it never writes, installs, trusts or runs Pi
// package-manager code. Everything is bounded and missing/linked/oversized
// inputs become diagnostics, never silent drops. Pi's models.json HTTP provider
// definitions are a documented non-goal; see docs/pi-compatibility.md.
import { lstatSync, readFileSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, extname, isAbsolute, join, resolve } from 'node:path';

const MAX_SETTINGS_BYTES = 1024 * 1024;
const MAX_THEME_BYTES = 256 * 1024;
const MAX_DIRECTORY_ENTRIES = 256;
const MAX_EXTENSIONS = 64;
const MAX_PACKAGES = 64;
const MAX_RESOURCE_ENTRIES = 64;
export const SOURCE_EXTENSIONS = new Set(['.ts', '.mts', '.cts', '.tsx', '.js', '.mjs', '.cjs', '.jsx']);
const CONTEXT_FILES = ['AGENTS.override.md', 'AGENTS.md', 'AGENTS.MD', 'CLAUDE.md', 'CLAUDE.MD'];
// Pi 1.0.2 resource kinds and conventional package directories.
const RESOURCE_KINDS = ['skills', 'prompts', 'themes'];

// `~` and relative entries resolve as Pi resolves them: against the user's home
// and the invocation directory. Unsupported pattern filters are refused with a
// diagnostic rather than guessing which resources Pi would enable.
export function expandPiPath(value, cwd) {
  if (value === '~') return homedir();
  if (value.startsWith('~/') || value.startsWith('~\\')) return join(homedir(), value.slice(2));
  return isAbsolute(value) ? value : resolve(cwd, value);
}

function isPlainPath(value) {
  return !value.startsWith('!') && !value.startsWith('+') && !value.startsWith('-');
}

function recordPath(list, seen, value, label, diagnostics) {
  if (list.length >= MAX_RESOURCE_ENTRIES) {
    diagnostics.push(`${label} ${value}: exceeds the ${MAX_RESOURCE_ENTRIES}-path mirror budget; skipped`);
    return;
  }
  if (seen.has(value)) return;
  seen.add(value);
  list.push(value);
}

function readJson(path, label, diagnostics, limit = MAX_SETTINGS_BYTES) {
  let stat;
  try { stat = lstatSync(path); } catch (error) {
    if (error.code !== 'ENOENT') diagnostics.push(`${label} ${path}: ${error.code || error.message}`);
    return undefined;
  }
  if (stat.isSymbolicLink() || !stat.isFile()) {
    if (!stat.isSymbolicLink()) return undefined;
    diagnostics.push(`${label} ${path}: symlinks are not mirrored`);
    return undefined;
  }
  if (stat.size > limit) {
    diagnostics.push(`${label} ${path}: exceeds the ${limit}-byte mirror limit`);
    return undefined;
  }
  try {
    const text = readFileSync(path, 'utf8');
    return JSON.parse(text.charCodeAt(0) === 0xfeff ? text.slice(1) : text);
  } catch (error) {
    diagnostics.push(`${label} ${path}: ${String(error.message || error).slice(0, 256)}`);
    return undefined;
  }
}

function directoryEntries(path, diagnostics) {
  let stat;
  try { stat = lstatSync(path); } catch (error) {
    if (error.code !== 'ENOENT') diagnostics.push(`directory ${path}: ${error.code || error.message}`);
    return [];
  }
  if (stat.isSymbolicLink()) { diagnostics.push(`directory ${path}: symlinks are not mirrored`); return []; }
  if (!stat.isDirectory()) return [];
  let names;
  try { names = readdirSync(path).sort(); } catch (error) { diagnostics.push(`directory ${path}: ${error.code || error.message}`); return []; }
  if (names.length > MAX_DIRECTORY_ENTRIES) {
    diagnostics.push(`directory ${path}: exceeds ${MAX_DIRECTORY_ENTRIES} entries; remaining entries were skipped`);
    names = names.slice(0, MAX_DIRECTORY_ENTRIES);
  }
  return names.map(name => join(path, name));
}

function regularFile(path, diagnostics, label) {
  let stat;
  try { stat = lstatSync(path); } catch (error) {
    if (error.code !== 'ENOENT') diagnostics.push(`${label} ${path}: ${error.code || error.message}`);
    return false;
  }
  if (stat.isSymbolicLink()) { diagnostics.push(`${label} ${path}: symlinks are not mirrored`); return false; }
  return stat.isFile();
}

function extensionEntries(paths, cwd, diagnostics, kind, out) {
  for (const entry of paths) {
    if (!isPlainPath(entry)) {
      diagnostics.push(`${kind} filter ${entry}: Pi resource filters are not mirrored; configure the path directly`);
      continue;
    }
    const path = expandPiPath(entry, cwd);
    let stat;
    try { stat = lstatSync(path); } catch (error) {
      if (error.code !== 'ENOENT') diagnostics.push(`${kind} ${path}: ${error.code || error.message}`);
      continue;
    }
    if (stat.isSymbolicLink()) { diagnostics.push(`${kind} ${path}: symlinks are not mirrored`); continue; }
    if (stat.isFile()) {
      if (SOURCE_EXTENSIONS.has(extname(path).toLowerCase())) out.push(path);
      else diagnostics.push(`${kind} ${path}: unsupported extension entry format`);
      continue;
    }
    if (stat.isDirectory()) {
      for (const child of directoryEntries(path, diagnostics)) {
        const childStat = (() => { try { return lstatSync(child); } catch { return undefined; } })();
        if (childStat?.isFile() && SOURCE_EXTENSIONS.has(extname(child).toLowerCase())) out.push(child);
      }
    }
  }
}

function resourceEntries(paths, cwd, diagnostics, kind, list, seen) {
  for (const entry of paths) {
    const text = String(entry);
    if (!isPlainPath(text)) {
      diagnostics.push(`${kind} filter ${text}: Pi resource filters are not mirrored; configure the path directly`);
      continue;
    }
    const path = expandPiPath(text, cwd);
    let stat;
    try { stat = lstatSync(path); } catch (error) {
      if (error.code !== 'ENOENT') diagnostics.push(`${kind} ${path}: ${error.code || error.message}`);
      continue;
    }
    if (stat.isSymbolicLink()) { diagnostics.push(`${kind} ${path}: symlinks are not mirrored`); continue; }
    if (!stat.isFile() && !stat.isDirectory()) {
      diagnostics.push(`${kind} ${path}: not a regular file or directory`);
      continue;
    }
    recordPath(list, seen, path, kind, diagnostics);
  }
}

// Pi-managed npm packages live under <agentDir>/npm/node_modules. The mirror
// resolves only packages already installed there; it never installs, updates or
// runs a package manager.
export function resolveManagedPackage(agentDir, source) {
  if (typeof source !== 'string') return undefined;
  const spec = source.startsWith('npm:') ? source.slice(4) : source;
  if (!spec || spec.startsWith('git:') || spec.startsWith('git@') || /^[a-z]+:\/\//i.test(spec)) return undefined;
  if (spec.startsWith('.') || spec.startsWith('/') || spec.startsWith('~/') || spec === '~') return undefined;
  return join(agentDir, 'npm', 'node_modules', spec);
}

function packageResources(agentDir, packages, cwd, diagnostics, setup, seen) {
  if (packages === undefined) return;
  if (!Array.isArray(packages)) { diagnostics.push('settings.packages is not an array; skipped'); return; }
  if (packages.length > MAX_PACKAGES) {
    diagnostics.push(`settings.packages exceeds ${MAX_PACKAGES} entries; remaining packages were skipped`);
  }
  for (const entry of packages.slice(0, MAX_PACKAGES)) {
    const source = typeof entry === 'string' ? entry : entry?.source;
    const filters = typeof entry === 'object' && entry !== null && !Array.isArray(entry) ? entry : {};
    if (typeof source !== 'string' || !source) { diagnostics.push('settings.packages entry has no source; skipped'); continue; }
    if (source.startsWith('git:') || source.startsWith('git@') || /^https?:\/\//i.test(source)) {
      diagnostics.push(`package ${source}: managed git packages are not mirrored; install it locally or configure its entrypoints explicitly`);
      continue;
    }
    const root = resolveManagedPackage(agentDir, source);
    if (!root) {
      diagnostics.push(`package ${source}: only absolute paths and managed npm packages are mirrored`);
      continue;
    }
    if (!existsDirectory(root)) {
      diagnostics.push(`package ${source}: not installed at ${root}; the mirror never installs packages`);
      continue;
    }
    const autoload = filters.autoload !== false;
    if (autoload) {
      for (const kind of RESOURCE_KINDS) resourceEntries([join(root, kind)], cwd, diagnostics, `${source} ${kind}`, setup[`${kind}Paths`], seen);
      extensionEntries([join(root, 'extensions')], cwd, diagnostics, `${source} extensions`, setup.extensions);
    }
    const declared = packageManifestResources(root, source, diagnostics);
    if (autoload && declared) {
      for (const kind of RESOURCE_KINDS) if (declared[kind]) resourceEntries(declared[kind], cwd, diagnostics, `${source} ${kind}`, setup[`${kind}Paths`], seen);
      if (declared.extensions) extensionEntries(declared.extensions, cwd, diagnostics, `${source} extensions`, setup.extensions);
    }
    for (const [field, kind] of [['extensions', 'extensions'], ['skills', 'skills'], ['prompts', 'prompts'], ['themes', 'themes']]) {
      const values = filters[field];
      if (values === undefined) continue;
      if (!Array.isArray(values) || values.some(value => typeof value !== 'string')) {
        diagnostics.push(`package ${source} ${field}: expected an array of relative paths; skipped`);
        continue;
      }
      const paths = values.map(value => value.endsWith('/**') ? resolve(root, value.slice(0, -3)) : resolve(root, value));
      if (kind === 'extensions') extensionEntries(paths, cwd, diagnostics, `${source} extensions`, setup.extensions);
      else resourceEntries(paths, cwd, diagnostics, `${source} ${kind}`, setup[`${kind}Paths`], seen);
    }
  }
}

function existsDirectory(path) {
  try { return lstatSync(path).isDirectory(); } catch { return false; }
}

function settingsPathList(settings, key, diagnostics) {
  const value = settings[key];
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.some(entry => typeof entry !== 'string')) {
    diagnostics.push(`settings.${key} must be an array of path strings; skipped`);
    return [];
  }
  return value;
}

// Pi's package manifest resources (`package.json` `pi` field) plus the
// conventional directories. Explicit entries are package-relative paths.
function packageManifestResources(root, source, diagnostics) {
  const manifest = readJson(join(root, 'package.json'), `package ${source} package.json`, diagnostics, 256 * 1024);
  const pi = manifest && typeof manifest === 'object' && !Array.isArray(manifest) ? manifest.pi : undefined;
  if (pi === undefined) return undefined;
  if (!pi || typeof pi !== 'object' || Array.isArray(pi)) {
    diagnostics.push(`package ${source}: package.json pi field must be an object; skipped`);
    return undefined;
  }
  const resources = {};
  for (const kind of ['extensions', ...RESOURCE_KINDS]) {
    const value = pi[kind];
    if (value === undefined) continue;
    if (!Array.isArray(value) || value.some(entry => typeof entry !== 'string')) {
      diagnostics.push(`package ${source} pi.${kind} must be an array of relative paths; skipped`);
      continue;
    }
    resources[kind] = value.map(entry => entry.endsWith('/**') ? resolve(root, entry.slice(0, -3)) : resolve(root, entry));
  }
  return Object.keys(resources).length ? resources : undefined;
}

/// Discover one machine's Pi setup. Pure read-only filesystem discovery with
/// explicit diagnostics; no Pi package code and no writes.
export function discoverPiSetup({ agentDir, env = process.env, cwd = process.cwd() } = {}) {
  const resolvedAgentDir = resolve(agentDir ?? env.OCTET_PI_AGENT_DIR ?? env.PI_CODING_AGENT_DIR ?? join(homedir(), '.pi', 'agent'));
  const diagnostics = [];
  const setup = {
    agentDir: resolvedAgentDir,
    settings: {},
    extensions: [],
    skillsPaths: [],
    promptsPaths: [],
    themesPaths: [],
    keybindingsPath: undefined,
    contextPath: undefined,
    defaultTheme: undefined,
    defaultThinkingLevel: undefined,
    defaultModel: undefined,
    diagnostics,
  };
  const seen = new Set();
  const settings = readJson(join(resolvedAgentDir, 'settings.json'), 'settings.json', diagnostics);
  if (settings !== undefined) {
    if (settings === null || typeof settings !== 'object' || Array.isArray(settings)) {
      diagnostics.push('settings.json: expected a JSON object; ignored');
    } else {
      setup.settings = settings;
      if (typeof settings.theme === 'string' && settings.theme.trim()) setup.defaultTheme = settings.theme.trim();
      if (typeof settings.defaultThinkingLevel === 'string' && settings.defaultThinkingLevel.trim()) {
        setup.defaultThinkingLevel = settings.defaultThinkingLevel.trim();
      }
      if (typeof settings.defaultProvider === 'string' && settings.defaultProvider.trim()
          && typeof settings.defaultModel === 'string' && settings.defaultModel.trim()) {
        setup.defaultModel = { provider: settings.defaultProvider.trim(), model: settings.defaultModel.trim() };
      }
    }
  }
  // Standard user resource directories first, then settings entries, then
  // packages: the native resolver owns final precedence, so order here is the
  // mirror's declaration order, not a replacement for native later-wins.
  const scopes = [resolvedAgentDir, join(cwd, '.pi')];
  const scopeSettings = [setup.settings];
  const projectSettings = readJson(join(cwd, '.pi', 'settings.json'), 'project settings.json', diagnostics);
  if (projectSettings !== undefined) {
    if (projectSettings === null || typeof projectSettings !== 'object' || Array.isArray(projectSettings)) {
      diagnostics.push('project settings.json: expected a JSON object; ignored');
    } else {
      scopeSettings.push(projectSettings);
    }
  }
  for (const scope of scopes) {
    const settings = scope === resolvedAgentDir ? scopeSettings[0] : scopeSettings[1] ?? {};
    for (const kind of RESOURCE_KINDS) resourceEntries([join(scope, kind)], cwd, diagnostics, kind, setup[`${kind}Paths`], seen);
    for (const kind of RESOURCE_KINDS) {
      resourceEntries(settingsPathList(settings, kind, diagnostics), cwd, diagnostics, kind, setup[`${kind}Paths`], seen);
    }
    extensionEntries([join(scope, 'extensions')], cwd, diagnostics, 'extensions', setup.extensions);
    extensionEntries(settingsPathList(settings, 'extensions', diagnostics), cwd, diagnostics, 'extensions', setup.extensions);
  }
  packageResources(resolvedAgentDir, setup.settings.packages, cwd, diagnostics, setup, seen);
  const projectPackages = scopeSettings[1]?.packages;
  if (projectPackages !== undefined) packageResources(resolvedAgentDir, projectPackages, cwd, diagnostics, setup, seen);
  // One entrypoint per file, in discovery order, even when a directory and a
  // settings/package entry name the same extension.
  setup.extensions = [...new Set(setup.extensions)];
  if (setup.extensions.length > MAX_EXTENSIONS) {
    diagnostics.push(`Pi setup has ${setup.extensions.length} extensions; only the first ${MAX_EXTENSIONS} are mirrored`);
    setup.extensions = setup.extensions.slice(0, MAX_EXTENSIONS);
  }
  const keybindings = join(resolvedAgentDir, 'keybindings.json');
  if (regularFile(keybindings, diagnostics, 'keybindings.json')) setup.keybindingsPath = keybindings;
  for (const name of CONTEXT_FILES) {
    const candidate = join(resolvedAgentDir, name);
    if (regularFile(candidate, diagnostics, 'context')) { setup.contextPath = candidate; break; }
  }
  return setup;
}

/// Resolve the Pi theme selection to one discovered JSON/TOML file. The file
/// stem is Octet's native selector; a Pi palette's `name` is the fallback.
export function resolveThemeFile(themePaths, selection, diagnostics = []) {
  if (!selection) return undefined;
  const files = themeCandidates(themePaths, diagnostics);
  const byStem = files.find(file => basename(file).replace(/\.(json|toml)$/i, '') === selection);
  if (byStem) return byStem;
  for (const file of files) {
    if (!/\.json$/i.test(file)) continue;
    let stat;
    try { stat = lstatSync(file); } catch { continue; }
    if (stat.size > MAX_THEME_BYTES) continue;
    try {
      const parsed = JSON.parse(readFileSync(file, 'utf8'));
      if (parsed && typeof parsed === 'object' && parsed.name === selection) return file;
    } catch { continue; }
  }
  return undefined;
}

/// Flatten discovered theme roots to individual palette files. Native theme
/// discovery treats a contributed directory and the file inside it as two
/// definitions of one name, so contributing the exact selected file as well
/// would register every palette twice (a shadow warning) and still be required
/// for `default_theme` admission. Contribute each palette once instead.
export function themeCandidates(paths, diagnostics = []) {
  const files = [], seen = new Set();
  for (const path of paths) {
    let stat;
    try { stat = lstatSync(path); } catch { continue; }
    if (stat.isFile()) {
      if (/\.(json|toml)$/i.test(path) && !seen.has(path) && files.length < MAX_RESOURCE_ENTRIES) {
        seen.add(path);
        files.push(path);
      }
      continue;
    }
    if (!stat.isDirectory()) continue;
    for (const child of directoryEntries(path, diagnostics)) {
      let childStat;
      try { childStat = lstatSync(child); } catch { continue; }
      if (childStat.isFile() && /\.(json|toml)$/i.test(child)) {
        if (!seen.has(child) && files.length < MAX_RESOURCE_ENTRIES) {
          seen.add(child);
          files.push(child);
        }
      } else if (childStat.isDirectory()) {
        diagnostics.push(`themes ${child}: nested theme directories are not mirrored; list the palette files explicitly`);
      }
    }
  }
  return files;
}

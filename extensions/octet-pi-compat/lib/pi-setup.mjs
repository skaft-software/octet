// Read-only mirror discovery for an existing Pi 1.0.2 setup.
// No package-manager calls, installs, factory execution, or reads of auth,
// sessions or trust files. Project scope requires explicit host trust.
import { closeSync, constants, fstatSync, lstatSync, openSync, readSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, dirname, extname, isAbsolute, join, matchesGlob, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const MAX_SETTINGS_BYTES = 1024 * 1024;
const MAX_THEME_BYTES = 256 * 1024;
const MAX_DIRECTORY_ENTRIES = 256;
const MAX_SCAN_ENTRIES = 4096;
const MAX_DEPTH = 16;
const MAX_EXTENSIONS = 64;
const MAX_PACKAGES = 64;
const MAX_RESOURCE_ENTRIES = 64;
export const SOURCE_EXTENSIONS = new Set(['.ts', '.mts', '.cts', '.tsx', '.js', '.mjs', '.cjs', '.jsx']);
const CONTEXT_FILES = ['AGENTS.override.md', 'AGENTS.md', 'AGENTS.MD', 'CLAUDE.md', 'CLAUDE.MD'];
const RESOURCE_KINDS = ['extensions', 'skills', 'prompts', 'themes'];
const plainObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const posix = value => value.split(sep).join('/');
const override = value => /^[!+-]/.test(value);
const glob = value => /[*?]/.test(value);
const within = (path, root) => path === root || path.startsWith(`${root}${sep}`);

// Pi settings paths are relative to agentDir or cwd/.pi, never cwd itself.
export function expandPiPath(value, baseDir) {
  value = value.trim();
  if (value === '~') return homedir();
  if (value.startsWith('~/') || value.startsWith('~\\')) return join(homedir(), value.slice(2));
  if (value.startsWith('file://')) return fileURLToPath(value);
  return isAbsolute(value) ? value : resolve(baseDir, value);
}

function diagnostic(ctx, message) { ctx.diagnostics.push(message); }

// Check every component: lstat on only the leaf still follows linked parents.
function safeStat(path, ctx, label, required = false) {
  path = resolve(path);
  if (ctx.projectRoot && !ctx.projectTrusted && within(path, ctx.projectRoot)) {
    diagnostic(ctx, `${label} ${path}: untrusted project .pi paths are not mirrored`);
    return undefined;
  }
  const ancestors = [];
  for (let part = path; ; part = dirname(part)) {
    ancestors.push(part);
    if (dirname(part) === part) break;
  }
  let stat;
  for (const part of ancestors.reverse()) {
    try { stat = lstatSync(part); } catch (error) {
      if (required || error.code !== 'ENOENT') diagnostic(ctx, `${label} ${path}: ${error.code === 'ENOENT' ? 'missing' : error.code || error.message}`);
      return undefined;
    }
    if (stat.isSymbolicLink()) {
      diagnostic(ctx, `${label} ${path}: symlinks are not mirrored (${part})`);
      return undefined;
    }
  }
  return stat;
}

function regularFile(path, ctx, label, limit = MAX_SETTINGS_BYTES, required = false) {
  const stat = safeStat(path, ctx, label, required);
  if (!stat) return false;
  if (!stat.isFile()) { diagnostic(ctx, `${label} ${path}: not a regular file`); return false; }
  if (stat.size > limit) { diagnostic(ctx, `${label} ${path}: exceeds the ${limit}-byte mirror limit`); return false; }
  return true;
}

function readJson(path, label, ctx, limit = MAX_SETTINGS_BYTES) {
  if (!regularFile(path, ctx, label, limit)) return undefined;
  let fd;
  try {
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size > limit) throw new Error(`not a regular file within the ${limit}-byte mirror limit`);
    const bytes = Buffer.alloc(limit + 1);
    let length = 0, count;
    while (length < bytes.length && (count = readSync(fd, bytes, length, bytes.length - length, null))) length += count;
    if (length > limit) throw new Error(`exceeds the ${limit}-byte mirror limit`);
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length));
    return JSON.parse(text.charCodeAt(0) === 0xfeff ? text.slice(1) : text);
  } catch (error) {
    diagnostic(ctx, `${label} ${path}: ${String(error.message || error).slice(0, 256)}`);
    return undefined;
  } finally { if (fd !== undefined) closeSync(fd); }
}

function directoryEntries(path, ctx, depth = 0) {
  if (depth > MAX_DEPTH || ctx.scanned >= MAX_SCAN_ENTRIES) {
    diagnostic(ctx, `directory ${path}: exceeds the bounded mirror traversal; skipped`);
    return [];
  }
  let names;
  try { names = readdirSync(path).sort(); } catch (error) { diagnostic(ctx, `directory ${path}: ${error.code || error.message}`); return []; }
  const remaining = Math.min(MAX_DIRECTORY_ENTRIES, MAX_SCAN_ENTRIES - ctx.scanned);
  if (names.length > remaining) diagnostic(ctx, `directory ${path}: exceeds the ${remaining}-entry mirror budget; remaining entries were skipped`);
  names = names.slice(0, remaining);
  ctx.scanned += names.length;
  return names.map(name => join(path, name));
}

// The adapter has no locked minimatch dependency. Node's matchesGlob honors
// Pi/minimatch's default semantics for this deliberately bounded subset. Other
// syntax fails closed for the whole resource kind, not to unfiltered defaults.
function validSelectors(values, label, ctx) {
  if (!Array.isArray(values) || values.length > MAX_DIRECTORY_ENTRIES || values.some(value => typeof value !== 'string' || !value || value.length > 4096 || /[\x00-\x1f\x7f\\{}[\]()]/.test(value))) {
    diagnostic(ctx, `${label}: expected a bounded array of path/selectors using only exact paths, *, ** and ?; resource kind skipped`);
    return false;
  }
  return true;
}

function matches(path, pattern, base, exact = false) {
  const candidates = [posix(relative(base, path)), posix(path)];
  if (!exact) candidates.push(basename(path));
  if (basename(path) === 'SKILL.md') {
    candidates.push(posix(relative(base, dirname(path))), posix(dirname(path)));
    if (!exact) candidates.push(basename(dirname(path)));
  }
  pattern = posix(pattern);
  if (exact && pattern.startsWith('./')) pattern = pattern.slice(2);
  return candidates.some(candidate => exact ? candidate === pattern : matchesGlob(candidate, pattern));
}

// Pi: includes, then !exclusions, then +exact inclusions, then -exact exclusions.
function enabledPaths(files, patterns, base) {
  const includes = patterns.filter(value => !override(value));
  const excludes = patterns.filter(value => value.startsWith('!')).map(value => value.slice(1));
  const plus = patterns.filter(value => value.startsWith('+')).map(value => value.slice(1));
  const minus = patterns.filter(value => value.startsWith('-')).map(value => value.slice(1));
  return new Set(files.filter(path => {
    let enabled = !includes.length || includes.some(pattern => matches(path, pattern, base));
    if (excludes.some(pattern => matches(path, pattern, base))) enabled = false;
    if (plus.some(pattern => matches(path, pattern, base, true))) enabled = true;
    if (minus.some(pattern => matches(path, pattern, base, true))) enabled = false;
    return enabled;
  }));
}

function manifestAt(root, ctx) {
  const path = join(root, 'package.json');
  const before = ctx.diagnostics.length;
  const manifest = readJson(path, 'package.json', ctx, MAX_THEME_BYTES);
  if (manifest === undefined && ctx.diagnostics.length > before) return { invalid: true };
  if (manifest === undefined) return {};
  if (!plainObject(manifest)) { diagnostic(ctx, `package ${root}: package.json must be an object; resources skipped`); return { invalid: true }; }
  if (manifest.pi === undefined) return {};
  if (!plainObject(manifest.pi)) { diagnostic(ctx, `package ${root}: package.json pi field must be an object; resources skipped`); return { invalid: true }; }
  return { pi: manifest.pi };
}

function hasIgnoreRules(path, ctx) {
  for (const name of ['.gitignore', '.ignore', '.fdignore']) {
    const before = ctx.diagnostics.length;
    if (safeStat(join(path, name), ctx, 'ignore rules') || ctx.diagnostics.length > before) {
      diagnostic(ctx, `directory ${path}: ${name} rules cannot be honored by the mirror; directory resources skipped`);
      return true;
    }
  }
  return false;
}

function acceptedFile(path, kind) {
  if (kind === 'extensions') return SOURCE_EXTENSIONS.has(extname(path).toLowerCase());
  if (kind === 'themes') return /\.(json|toml)$/i.test(path);
  return /\.md$/i.test(path);
}

function collect(path, kind, ctx, { required = false, depth = 0, root = path, nestedEntry = false } = {}) {
  const stat = safeStat(path, ctx, kind, required);
  if (!stat) return [];
  if (stat.isFile()) {
    if (!acceptedFile(path, kind)) { if (required) diagnostic(ctx, `${kind} ${path}: unsupported resource format`); return []; }
    return regularFile(path, ctx, kind, kind === 'themes' ? MAX_THEME_BYTES : MAX_SETTINGS_BYTES) ? [path] : [];
  }
  if (!stat.isDirectory()) { diagnostic(ctx, `${kind} ${path}: not a regular file or directory`); return []; }
  if (depth > MAX_DEPTH) { diagnostic(ctx, `${kind} ${path}: exceeds ${MAX_DEPTH} directory levels; skipped`); return []; }
  if (kind === 'extensions') {
    const manifest = manifestAt(path, ctx);
    if (manifest.invalid) return [];
    if (manifest.pi?.extensions !== undefined) return manifestFiles(path, kind, manifest.pi.extensions, ctx, depth + 1);
    for (const name of ['index.ts', 'index.js', 'index.mjs', 'index.cjs']) {
      const index = join(path, name), before = ctx.diagnostics.length;
      if (safeStat(index, ctx, kind)) return collect(index, kind, ctx, { required: true, depth: depth + 1 });
      if (ctx.diagnostics.length > before) return []; // never replace a refused entrypoint with another factory
    }
    if (nestedEntry) { diagnostic(ctx, `extensions ${path}: directory has no package entrypoint or index.ts/index.js; skipped`); return []; }
  }
  if (hasIgnoreRules(path, ctx)) return [];
  if (kind === 'skills') {
    const skill = join(path, 'SKILL.md'), before = ctx.diagnostics.length;
    if (safeStat(skill, ctx, kind)) return collect(skill, kind, ctx, { required: true });
    if (ctx.diagnostics.length > before) return [];
  }
  const out = [];
  for (const child of directoryEntries(path, ctx, depth)) {
    if (basename(child).startsWith('.') || basename(child) === 'node_modules') continue;
    const childStat = safeStat(child, ctx, kind);
    if (!childStat) continue;
    if (kind === 'skills' && childStat.isFile() && path !== root) continue;
    out.push(...collect(child, kind, ctx, { depth: depth + 1, root, nestedEntry: kind === 'extensions' && childStat.isDirectory() }));
  }
  return [...new Set(out)];
}

function expandGlob(pattern, root, ctx, depth = 0) {
  const out = [];
  const walk = (dir, level) => {
    for (const path of directoryEntries(dir, ctx, level)) {
      if (basename(path).startsWith('.') || basename(path) === 'node_modules') continue;
      const stat = safeStat(path, ctx, 'manifest glob');
      if (!stat) continue;
      if (matchesGlob(posix(relative(root, path)), pattern)) out.push(path);
      if (stat.isDirectory() && level < MAX_DEPTH) walk(path, level + 1);
      else if (stat.isDirectory()) diagnostic(ctx, `manifest glob ${path}: exceeds ${MAX_DEPTH} directory levels; skipped`);
    }
  };
  walk(root, depth);
  if (!out.length) diagnostic(ctx, `manifest glob ${pattern} in ${root}: no installed resources matched`);
  return out;
}

function manifestFiles(root, kind, entries, ctx, depth = 0) {
  if (!validSelectors(entries, `package ${root} pi.${kind}`, ctx)) return [];
  const files = [];
  for (const entry of entries.filter(value => !override(value))) {
    let paths;
    try { paths = glob(entry) ? expandGlob(entry, root, ctx, depth) : [expandPiPath(entry, root)]; }
    catch (error) { diagnostic(ctx, `${kind} ${entry}: ${error.message}`); return []; }
    for (const path of paths) {
      if (path === root && kind === 'extensions') { diagnostic(ctx, `extensions ${root}: recursive manifest entry skipped`); continue; }
      files.push(...collect(path, kind, ctx, { required: true, depth: depth + 1 }));
    }
  }
  return [...enabledPaths([...new Set(files)], entries.filter(override), root)];
}

function parseNpm(source) {
  if (!source.startsWith('npm:')) return undefined;
  const spec = source.slice(4).trim();
  // Same name/version split as Pi's parseNpmSpec; validate the managed path.
  const match = spec.match(/^(@?[^@]+(?:\/[^@]+)?)(?:@(.+))?$/);
  if (!match) return undefined;
  const [, name, version] = match;
  if (!/^(?:@[A-Za-z0-9_~.-]+\/)?[A-Za-z0-9_~.-]+$/.test(name) || name.split('/').some(part => part === '.' || part === '..')) return undefined;
  return { name, version };
}

// Exact SemVer only. There is no locked JS semver dependency in this adapter;
// ranges and tags are diagnosed and refused, never approximated or installed.
function exactVersion(value) {
  if (typeof value !== 'string' || value.length > 256) return undefined;
  const match = value.trim().match(/^v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/);
  if (!match || match.slice(1, 4).some(part => !Number.isSafeInteger(Number(part))) || match[4]?.split('.').some(part => /^\d+$/.test(part) && part.length > 1 && part.startsWith('0'))) return undefined;
  return `${match[1]}.${match[2]}.${match[3]}${match[4] ? `-${match[4]}` : ''}`;
}

export function resolveManagedPackage(baseDir, source) {
  const npm = typeof source === 'string' ? parseNpm(source) : undefined;
  return npm ? join(baseDir, 'npm', 'node_modules', npm.name) : undefined;
}

function parseGit(source) {
  const prefixed = source.startsWith('git:');
  let text = prefixed ? source.slice(4).trim() : source;
  if (!prefixed && !/^(https?|ssh|git):\/\//i.test(text)) return undefined;
  // Explicit host/path and protocol/SSH forms. Hosted-git-info aliases are not
  // guessed when its dependency is absent.
  const scp = text.match(/^git@([^:]+):(.+)$/);
  let host, path;
  if (scp) [, host, path] = scp;
  else if (/^(https?|ssh|git):\/\//i.test(text)) {
    let url;
    try { url = new URL(text); } catch { return undefined; }
    host = url.hostname; path = url.pathname.replace(/^\/+/, '');
  } else {
    const slash = text.indexOf('/');
    if (slash < 0) return undefined;
    host = text.slice(0, slash); path = text.slice(slash + 1);
    if (!host.includes('.') && host !== 'localhost') return undefined;
  }
  path = path.split('@')[0].replace(/\.git$/, '').replace(/\/$/, '');
  let decoded;
  try { decoded = decodeURIComponent(path); } catch { return undefined; }
  if (!/^[A-Za-z0-9.-]+$/.test(host) || host === '.' || host === '..' || !path || path.split('/').length < 2 || [path, decoded].some(value => /[\x00-\x1f\\]/.test(value) || value.split('/').some(part => !part || part === '.' || part === '..'))) return undefined;
  return { host, path };
}

function sourceInfo(source, base) {
  const npm = parseNpm(source);
  if (npm) return { type: 'npm', ...npm, root: resolveManagedPackage(base, source), identity: `npm:${npm.name}` };
  if (source.startsWith('npm:')) return undefined;
  const git = parseGit(source);
  if (git) return { type: 'git', root: join(base, 'git', git.host, git.path), identity: `git:${git.host}/${git.path}` };
  if (/^(?:git:|git@|[A-Za-z][A-Za-z0-9+.-]*:)/.test(source) && !source.startsWith('file://')) return undefined;
  try {
    const root = expandPiPath(source, base);
    return { type: 'local', root, identity: `local:${root}` };
  } catch { return undefined; }
}

function packageEntries(settings, base, scope, ctx) {
  const packages = settings.packages;
  if (packages === undefined) return [];
  if (!Array.isArray(packages)) { diagnostic(ctx, 'settings.packages is not an array; skipped'); return []; }
  if (packages.length > MAX_PACKAGES) diagnostic(ctx, `settings.packages exceeds ${MAX_PACKAGES} entries; remaining packages were skipped`);
  return packages.slice(0, MAX_PACKAGES).flatMap(pkg => {
    const source = typeof pkg === 'string' ? pkg : plainObject(pkg) ? pkg.source : undefined;
    if (typeof source !== 'string' || !source.trim() || source.length > 4096 || /[\x00-\x1f\x7f]/.test(source)) { diagnostic(ctx, 'settings.packages entry has no bounded source; skipped'); return []; }
    const info = sourceInfo(source.trim(), base);
    if (!info) { diagnostic(ctx, `package ${source}: unsupported source syntax; not mirrored`); return []; }
    return [{ pkg, source, info, scope }];
  });
}

function add(map, path, enabled, rank = 4) { if (!map.has(path)) map.set(path, { enabled, rank }); }

function packageResources(entries, maps, ctx) {
  const deduped = [], seen = new Map();
  // Project wins by identity, ignoring npm versions and git refs. A project
  // autoload:false entry is a delta against the global installation instead.
  for (const entry of entries) {
    const existing = seen.get(entry.info.identity);
    if (!existing) { seen.set(entry.info.identity, entry); deduped.push(entry); }
    else if (existing.scope === 'project' && entry.scope === 'user' && existing.pkg?.autoload === false) deduped.push(entry);
  }
  for (const entry of deduped) {
    const filter = plainObject(entry.pkg) ? entry.pkg : undefined;
    const delta = filter?.autoload === false;
    const base = entry.scope === 'project' && delta ? deduped.find(other => other.scope === 'user' && other.info.identity === entry.info.identity) ?? entry : entry;
    const { root, type, version } = base.info;
    const stat = safeStat(root, ctx, `package ${entry.source}`);
    if (!stat) { diagnostic(ctx, `package ${entry.source}: not installed at ${root}; the mirror never installs packages`); continue; }
    if (type === 'npm') {
      const manifest = readJson(join(root, 'package.json'), `package ${entry.source}`, ctx, MAX_THEME_BYTES);
      const installed = exactVersion(manifest?.version);
      if (!installed) { diagnostic(ctx, `package ${entry.source}: installed package.json has no valid version; skipped`); continue; }
      if (version !== undefined) {
        const expected = exactVersion(version);
        if (!expected) { diagnostic(ctx, `package ${entry.source}: version range/tag ${version} cannot be verified without a locked semver dependency; skipped`); continue; }
        if (expected !== installed) { diagnostic(ctx, `package ${entry.source}: installed version ${manifest.version} does not match ${version}; the mirror never installs packages`); continue; }
      }
    }
    // Local-file sources are singleton extension packages, but still obey the
    // same selectors and autoload/project deltas before any enabled insertion.
    const sourceFile = type === 'local' && stat.isFile();
    if (!sourceFile && !stat.isDirectory()) { diagnostic(ctx, `package ${entry.source}: not a package directory; skipped`); continue; }
    const manifest = sourceFile ? {} : manifestAt(root, ctx);
    if (manifest.invalid) continue;
    const hasConventional = !sourceFile && RESOURCE_KINDS.some(kind => safeStat(join(root, kind), ctx, kind)?.isDirectory());
    for (const kind of sourceFile ? ['extensions'] : RESOURCE_KINDS) {
      const patterns = filter?.[kind];
      if (patterns !== undefined && !validSelectors(patterns, `package ${entry.source} ${kind}`, ctx)) {
        // A project delta with unsupported selectors must also suppress the
        // global kind; otherwise global autoload would bypass the restriction.
        const files = sourceFile ? collect(root, kind, ctx, { required: true }) : manifest.pi?.[kind] !== undefined ? manifestFiles(root, kind, manifest.pi[kind], ctx) : collect(join(root, kind), kind, ctx);
        for (const path of files) add(maps[kind], path, false);
        continue;
      }
      let files;
      if (sourceFile) files = collect(root, kind, ctx, { required: true });
      else if (manifest.pi?.[kind] !== undefined) files = manifestFiles(root, kind, manifest.pi[kind], ctx);
      else if (manifest.pi && !filter) files = [];
      else if (type === 'local' && !manifest.pi && !hasConventional && kind === 'extensions') {
        files = collect(root, kind, ctx);
        if (!files.length) diagnostic(ctx, `package ${entry.source}: no supported installed extension entrypoints; skipped`);
      }
      else files = collect(join(root, kind), kind, ctx);
      if (delta) {
        const changes = new Map();
        for (const pattern of patterns ?? []) {
          const exact = /^[+-]/.test(pattern);
          const target = override(pattern) ? pattern.slice(1) : pattern;
          const enabled = !/^[!-]/.test(pattern);
          // Deltas are ordered; last matching selector wins within this entry.
          for (const path of files) if (matches(path, target, root, exact)) {
            changes.set(path, enabled);
          }
        }
        for (const [path, enabled] of changes) add(maps[kind], path, enabled);
      } else {
        const enabled = patterns === undefined ? new Set(files) : patterns.length === 0 ? new Set() : enabledPaths(files, patterns, root);
        for (const path of files) add(maps[kind], path, enabled.has(path));
      }
    }
  }
}

function settingsObject(path, label, ctx) {
  const before = ctx.diagnostics.length;
  const settings = readJson(path, label, ctx);
  if (settings === undefined) return ctx.diagnostics.length === before ? {} : undefined;
  if (!plainObject(settings)) { diagnostic(ctx, `${label}: expected a JSON object; scope resources skipped`); return undefined; }
  return settings;
}

function mergeSettings(base, overrides) {
  const merged = Object.fromEntries(Object.entries(base));
  for (const [key, value] of Object.entries(overrides)) {
    Object.defineProperty(merged, key, { value: plainObject(base[key]) && plainObject(value) ? mergeSettings(base[key], value) : value, enumerable: true, writable: true, configurable: true });
  }
  if (Array.isArray(base.defaultTools) && Array.isArray(overrides.defaultTools) && overrides.defaultTools.every(value => typeof value === 'string' && /^[+-]/.test(value))) merged.defaultTools = [...base.defaultTools, ...overrides.defaultTools];
  return merged;
}

// Contract: extensions and resource paths are exact enabled files, not roots
// that a downstream loader could re-expand to excluded or linked resources.
// projectTrusted defaults false, even for a reviewed global mirror. Callers
// must pass the host's current project decision, never infer trust from opt-in.
export function discoverPiSetup({ agentDir, env = process.env, cwd = process.cwd(), projectTrusted = false } = {}) {
  const resolvedAgentDir = resolve(agentDir ?? env.OCTET_PI_AGENT_DIR ?? env.PI_CODING_AGENT_DIR ?? join(homedir(), '.pi', 'agent'));
  const projectRoot = resolve(cwd, '.pi');
  const ctx = { diagnostics: [], scanned: 0, projectRoot, projectTrusted: projectTrusted === true };
  const globalSettings = settingsObject(join(resolvedAgentDir, 'settings.json'), 'settings.json', ctx);
  const projectSettings = ctx.projectTrusted ? settingsObject(join(projectRoot, 'settings.json'), 'project settings.json', ctx) : {};
  const settings = mergeSettings(globalSettings ?? {}, projectSettings ?? {});
  const setup = { agentDir: resolvedAgentDir, settings, extensions: [], skillsPaths: [], promptsPaths: [], themesPaths: [], keybindingsPath: undefined, contextPath: undefined, defaultTheme: undefined, defaultThinkingLevel: undefined, defaultModel: undefined, diagnostics: ctx.diagnostics };
  const text = value => typeof value === 'string' && value.trim() ? value.trim() : undefined;
  setup.defaultTheme = text(settings.theme);
  setup.defaultThinkingLevel = text(settings.defaultThinkingLevel);
  if (text(settings.defaultProvider) && text(settings.defaultModel)) setup.defaultModel = { provider: text(settings.defaultProvider), model: text(settings.defaultModel) };
  const maps = Object.fromEntries(RESOURCE_KINDS.map(kind => [kind, new Map()]));
  const packages = [
    ...packageEntries(projectSettings ?? {}, projectRoot, 'project', ctx),
    ...packageEntries(globalSettings ?? {}, resolvedAgentDir, 'user', ctx),
  ];
  packageResources(packages, maps, ctx);
  // Pi's first declaration wins for the same path (including disabled paths).
  // Return Pi's precedence order: project explicit/auto, user explicit/auto,
  // then packages. Native discovery still owns name collisions and admission.
  const scopes = (ctx.projectTrusted ? [[projectRoot, projectSettings], [resolvedAgentDir, globalSettings]] : [[resolvedAgentDir, globalSettings]])
    .filter(([, scopeSettings]) => scopeSettings !== undefined);
  for (const [base, scopeSettings] of scopes) {
    for (const kind of RESOURCE_KINDS) {
      const entries = scopeSettings[kind] ?? [];
      if (!validSelectors(entries, `settings.${kind}`, ctx)) continue;
      const patterns = entries.filter(value => override(value) || glob(value));
      const local = [];
      for (const value of entries.filter(value => !override(value) && !glob(value))) {
        try { local.push(...collect(expandPiPath(value, base), kind, ctx, { required: true })); }
        catch (error) { diagnostic(ctx, `${kind} ${value}: ${error.message}`); }
      }
      const enabled = enabledPaths(local, patterns, base);
      for (const path of local) add(maps[kind], path, enabled.has(path), base === projectRoot ? 0 : 2);
      const auto = collect(join(base, kind), kind, ctx);
      const autoEnabled = enabledPaths(auto, patterns.filter(override), base);
      for (const path of auto) add(maps[kind], path, autoEnabled.has(path), base === projectRoot ? 1 : 3);
    }
  }
  for (const kind of RESOURCE_KINDS) {
    const files = [...maps[kind]].filter(([, value]) => value.enabled).sort((a, b) => a[1].rank - b[1].rank).map(([path]) => path);
    const limit = kind === 'extensions' ? MAX_EXTENSIONS : MAX_RESOURCE_ENTRIES;
    if (files.length > limit) diagnostic(ctx, `Pi setup has ${files.length} ${kind}; only the first ${limit} are mirrored`);
    setup[kind === 'extensions' ? kind : `${kind}Paths`] = files.slice(0, limit);
  }
  const keybindings = join(resolvedAgentDir, 'keybindings.json');
  if (regularFile(keybindings, ctx, 'keybindings.json')) setup.keybindingsPath = keybindings;
  for (const name of CONTEXT_FILES) {
    const path = join(resolvedAgentDir, name);
    if (regularFile(path, ctx, 'context')) { setup.contextPath = path; break; }
  }
  return setup;
}

export function resolveThemeFile(themePaths, selection, diagnostics = []) {
  if (!selection) return undefined;
  const files = themeCandidates(themePaths, diagnostics);
  const byStem = files.find(file => basename(file).replace(/\.(json|toml)$/i, '') === selection);
  if (byStem) return byStem;
  const ctx = { diagnostics, scanned: 0 };
  for (const file of files) {
    if (!/\.json$/i.test(file)) continue;
    const parsed = readJson(file, 'theme', ctx, MAX_THEME_BYTES);
    if (plainObject(parsed) && parsed.name === selection) return file;
  }
  return undefined;
}

// Also accepts directly configured roots. No linked/oversized palettes escape
// through this public helper after discovery's own checks.
export function themeCandidates(paths, diagnostics = []) {
  const ctx = { diagnostics, scanned: 0 };
  const files = [...new Set(paths.flatMap(path => collect(path, 'themes', ctx, { required: true })))];
  if (files.length > MAX_RESOURCE_ENTRIES) diagnostic(ctx, `themes: exceeds the ${MAX_RESOURCE_ENTRIES}-path mirror budget; remaining palettes skipped`);
  return files.slice(0, MAX_RESOURCE_ENTRIES);
}

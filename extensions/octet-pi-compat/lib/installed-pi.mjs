// Opt-in "installed Pi" runtime (path B). Path A (the default) aliases Pi
// package names to Octet's shims. Path B is selected only by an explicit
// `pi_runtime: "installed"` bridge field or `--pi-runtime installed`, after the
// host obtained the user's approval. It never activates implicitly.
//
// Path B keeps every Octet shim export (shim names always win, so the host
// boundary for sessions, providers, UI, settings, tools and the `pi` object is
// unchanged) and fills only names the shims lack from the user's managed Pi
// install. Real Pi names that would take over Octet-owned side effects are
// refused, and real names this build has not classified are refused too, so a
// export from another Pi release cannot silently become live. This is a guard against
// silent takeover, not a sandbox: a trusted factory keeps normal OS authority.
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, extname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { invalid, rpcError } from './errors.mjs';
import { PI_MODULES, PI_PREFIXES, PI_VERSION, piModule } from './pi-modules.mjs';

export const FALLBACK_ELIGIBLE = -32030;
export const INSTALLED_PI_UNAVAILABLE = -32031;
// octet 0.8.2 is pinned to exactly the Pi release its shims and module table
// were built from; another installed Pi version is refused, not guessed at.
export const SUPPORTED_RANGE = PI_VERSION;
const SUPPORTED = { test: version => version === PI_VERSION };
const REGISTRY = Symbol.for('octet.pi-compat.installed-pi');
const PREFIXES = PI_PREFIXES;
// shim file key -> real package directory name
export const PACKAGES = { 'coding-agent': 'pi-coding-agent', ai: 'pi-ai', tui: 'pi-tui' };
// Pi modules octet does not emulate: the fallback serves the installed files
// as they are, with the export Pi 1.0.2's loader resolves them to.
const DIRECT_MODULES = Object.fromEntries(Object.entries(PI_MODULES).filter(([, module]) => !module.shim));
// The export each emulated package's real namespace is read from (Pi resolves
// the pi-ai root to its compat entry, a strict superset of the core entry).
const REAL_EXPORT = { 'coding-agent': '.', ai: './compat', tui: '.' };
const words = text => text.trim().split(/\s+/);

// Real-only names (absent from the shims) refused in path B, with the seam each
// one would open. Keep in sync with NOTES/docs; pinned to Pi 1.0.2.
const DENIED = {
  'coding-agent': [
    ['AgentSessionRuntime createAgentSessionFromServices createAgentSessionRuntime createAgentSessionServices createExtensionRuntime ExtensionRunner discoverAndLoadExtensions InteractiveMode main runPrintMode runRpcMode RpcClient wrapRegisteredTool wrapRegisteredTools',
      'Pi sessions, agent runtimes and extension runners would bypass the Octet-owned agent loop and session persistence'],
    ['ModelRuntime readStoredCredential LoginDialogComponent OAuthSelectorComponent compact generateSummary generateSummaryWithUsage generateBranchSummary',
      'Pi provider calls and credential storage would bypass Octet-owned inference and credentials'],
    ['ProjectTrustStore DefaultPackageManager initTheme',
      'Pi config, trust, package and theme state would be read or written instead of Octet state'],
    ['createMcpExtension createCodemodeExtension createToolSearchExtension',
      'Pi MCP/codemode runtimes would start servers outside the Octet-owned MCP registry'],
    ['copyToClipboard', 'the clipboard and terminal are Octet-owned'],
  ],
  ai: [
    ['createModels createProvider defaultProviderAuthContext envApiKeyAuth lazyApi lazyOAuth lazyStream retryAssistantCall InMemoryCredentialStore InMemoryModelsStore',
      'Pi provider runtimes would make model calls with the user\'s credentials outside Octet-owned inference'],
  ],
  tui: [
    ['StdinBuffer TuiAltScreen TuiMainScreen getNativeClipboard', 'terminal input, screen ownership and the clipboard are Octet-owned'],
  ],
};
// Real-only names admitted in path B: pure helpers, data, and components that
// render into the host-mediated remote UI.
const ALLOWED = {
  'coding-agent': words(`ArminComponent AssistantMessageComponent BashExecutionComponent BranchSummaryMessageComponent CURRENT_SESSION_VERSION
    CompactionSummaryMessageComponent CredentialSynchronizationError CustomMessageComponent DEFAULT_COMPACTION_SETTINGS ExtensionEditorComponent
    ExtensionInputComponent ExtensionSelectorComponent FooterComponent ModelSelectorComponent SessionSelectorComponent SettingsSelectorComponent
    ShowImagesSelectorComponent SkillInvocationMessageComponent Theme ThemeSelectorComponent ThinkingSelectorComponent ToolExecutionComponent
    TreeSelectorComponent UserMessageComponent UserMessageSelectorComponent VIRTUAL_MODEL_STATE_ENTRY buildContextEntries buildSessionProjection
    collectEntriesForBranchSummary convertToPng createEventBus createSyntheticSourceInfo detectSupportedImageMimeTypeFromFile findCutPoint
    findTurnStartIndex formatDimensionNote formatSkillsForPrompt generateDiffString generateUnifiedPatch getDocsPath getExamplesPath
    getLanguageFromPath getLastAssistantUsage getLatestCompactionEntry getPackageDir getPowerShellConfig getReadmePath getShellConfig
    hasTrustRequiringProjectResources highlightCode keyText loadProjectContextFiles loadSkills loadSkillsFromDir migrateSessionEntries parseArgs
    parseSessionEntries parseSkillBlock prepareBranchEntries rawKeyHint renderDiff resizeImage resolveCliModel resolveModelScopeWithDiagnostics
    sessionEntryToContextMessages shouldCompact truncateToVisualLines`),
  ai: words(`AssistantMessageEventStream AssistantMessageFrameEncoder DEFAULT_MAX_AGENT_RETRY_DELAY_MS EventStream ModelsError
    appendAssistantMessageDiagnostic clampThinkingLevel cleanupSessionResources contentText createAssistantMessageDiagnostic createFauxCore
    createInitialSystemMessage declarationsEqual extractDiagnosticError fauxAssistantMessage fauxProvider fauxText fauxThinking fauxToolCall
    formatThrownValue getCurrentSystemMessage getDeclaredTools getInitialSystemMessage getModelType getOverflowPatterns getSupportedThinkingLevels
    getSystemMessageText getToolStateChanges hasApi hasNonAdditiveToolChanges hasToolRedefinitions isContextOverflow isModelType isRecoverableLength
    isRetryableAssistantError modelsAreEqual normalizeContext parseJsonWithRepair parseStreamingJson reduceAssistantMessageFrames
    registerSessionResourceCleanup renderSystemMessageUpdate repairJson resolveTranscript resolveTranscriptTools retryDelayMs toToolDeclaration
    validateToolArguments validateToolCall withoutInitialSystemMessage`),
  tui: words(`CombinedAutocompleteProvider HStack Marked MouseRegion ScrollView VStack allocateImageId backgroundAnsi calculateImageRows colorToHex
    colorToOkhsl colorToOklch colorToRgb compositeTuiLine deleteAllKittyImages deleteKittyImage detectCapabilities encodeITerm2 encodeKitty
    foregroundAnsi getCapabilities getCellDimensions getGifDimensions getImageDimensions getJpegDimensions getOsc8LinkAtColumn getPngDimensions
    getTerminalColorMode getWebpDimensions hyperlink imageFallback indexedColor isAppleTerminalSession isViewportTUI mixColors okhslColor
    oklabToOkhslLightness oklchColor parseColor parseTerminalColorSchemeReport renderImage renderLatex resetCapabilitiesCache rgbColor
    setCapabilities setCapabilityOverrides setCellDimensions setImageTranscoder styleText styleTextWithAnsi`),
};
// Pi built-in tool factories run commands and file I/O inside the extension
// process, outside Octet's per-command tool policy. Path A refuses them; the
// user-approved fallback exists for exactly this, so real Pi wins over the shim.
const REAL_OVERRIDES = {
  'coding-agent': new Set(words(`createBashTool createBashToolDefinition createCodingTools createEditTool createEditToolDefinition createFindTool
    createFindToolDefinition createGrepTool createGrepToolDefinition createLocalBashOperations createLocalPowerShellOperations createLsTool
    createLsToolDefinition createPowerShellTool createPowerShellToolDefinition createReadOnlyTools createReadTool createReadToolDefinition
    createWriteTool createWriteToolDefinition`)),
  ai: new Set(), tui: new Set(),
};
// A Pi specifier the emulated path cannot serve: a module octet does not
// emulate (agent core, OAuth, provider registry) or an unmapped subpath.
const fallbackSubpath = specifier => { const pi = piModule(specifier); return !!pi && (pi.unmapped || !pi.shim); };
const deniedReason = Object.fromEntries(Object.entries(DENIED).map(([pkg, groups]) =>
  [pkg, new Map(groups.flatMap(([names, reason]) => words(names).map(name => [name, reason])))]));
const allowed = Object.fromEntries(Object.entries(ALLOWED).map(([pkg, names]) => [pkg, new Set(names)]));

export function piRuntimeMode(config) {
  const mode = config.pi_runtime ?? 'shims';
  if (mode !== 'shims' && mode !== 'installed') invalid('pi_runtime must be "shims" or "installed"');
  return mode;
}
export const shimPath = key => fileURLToPath(new URL(`../shims/${key}.mjs`, import.meta.url));
const overlayPath = key => fileURLToPath(new URL(`./installed-pi/${key}.cjs`, import.meta.url));
const unavailable = message => rpcError(INSTALLED_PI_UNAVAILABLE, `installed_pi_unavailable ${message}; refusing the installed-Pi fallback (no silent fallback)`);
const inside = (parent, child) => { const r = relative(parent, child); return r === '' || (!!r && !r.startsWith('..') && !isAbsolute(r)); };

// Mirrors ~/.pi/agent/bin/pi: <agent>/install/current-version names a release
// under <agent>/install/releases/<version>. OCTET_PI_AGENT_DIR overrides the
// agent directory (the launcher derives it from its own location).
export function locateInstalledPi(env = process.env) {
  const agentDir = env.OCTET_PI_AGENT_DIR ? resolve(env.OCTET_PI_AGENT_DIR) : join(homedir(), '.pi', 'agent');
  const versionFile = join(agentDir, 'install', 'current-version');
  let raw;
  try { raw = readFileSync(versionFile, 'utf8'); } catch (error) { throw unavailable(`could not read managed Pi version from ${versionFile} (${error.code || error.message})`); }
  const version = raw.split('\n', 1)[0];
  if (!version || version === '.' || version === '..' || !/^[0-9A-Za-z._+-]+$/.test(version)) throw unavailable(`managed Pi version file is invalid: ${versionFile}`);
  if (!SUPPORTED.test(version)) throw unavailable(`installed Pi ${version} is outside the supported range ${SUPPORTED_RANGE}`);
  let releaseDir;
  try { releaseDir = realpathSync(join(agentDir, 'install', 'releases', version)); } catch { throw unavailable(`managed Pi release ${version} is missing under ${join(agentDir, 'install', 'releases')}`); }
  // Every package the module table names must be the pinned release.
  const manifests = {};
  const packageDir = name => {
    if (manifests[name]) return manifests[name];
    const declared = join(releaseDir, 'node_modules', '@earendil-works', name);
    let dir, pkg;
    try { dir = realpathSync(declared); pkg = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8')); }
    catch (error) { throw unavailable(`@earendil-works/${name} is missing or unreadable in Pi ${version} (${error.code || error.message})`); }
    if (!inside(releaseDir, dir)) throw unavailable(`@earendil-works/${name} resolves outside the managed Pi release`);
    if (pkg.name !== `@earendil-works/${name}` || pkg.version !== version) throw unavailable(`@earendil-works/${name} reports ${pkg.name}@${pkg.version}, expected ${version}`);
    return (manifests[name] = { dir, pkg });
  };
  const exportFile = (name, exportKey) => {
    const { dir, pkg } = packageDir(name);
    const pickTarget = target => typeof target === 'string' ? target : target?.import ?? target?.default;
    let relativeEntry;
    if (exportKey === '.') relativeEntry = pickTarget(pkg.exports?.['.'] ?? (typeof pkg.exports === 'string' ? pkg.exports : undefined)) ?? pkg.main ?? 'index.js';
    else if (pkg.exports && Object.hasOwn(pkg.exports, exportKey)) relativeEntry = pickTarget(pkg.exports[exportKey]);
    else {
      // Subpath patterns, as Node resolves them ("./providers/*").
      for (const [pattern, target] of Object.entries(pkg.exports ?? {})) {
        const star = pattern.indexOf('*');
        if (star < 0) continue;
        const [head, tail] = [pattern.slice(0, star), pattern.slice(star + 1)];
        if (!exportKey.startsWith(head) || !exportKey.endsWith(tail) || exportKey.length < pattern.length - 1) continue;
        const match = exportKey.slice(head.length, exportKey.length - tail.length);
        relativeEntry = pickTarget(target)?.replaceAll('*', match);
        break;
      }
    }
    if (!relativeEntry) throw unavailable(`@earendil-works/${name} does not export ${exportKey} in Pi ${version}`);
    const file = resolve(dir, relativeEntry);
    try { if (!inside(dir, realpathSync(file)) || !statSync(file).isFile()) throw new Error('not a file inside the package'); }
    catch (error) { throw unavailable(`@earendil-works/${name} export ${exportKey} (${relativeEntry}) is invalid (${error.code || error.message})`); }
    return file;
  };
  const packages = {};
  for (const [key, name] of Object.entries(PACKAGES)) {
    packages[key] = { name: `@earendil-works/${name}`, dir: packageDir(name).dir, entry: exportFile(name, REAL_EXPORT[key]), version };
  }
  // Modules without a shim resolve to the installed files directly.
  const direct = {};
  for (const [specifier, module] of Object.entries(DIRECT_MODULES)) direct[specifier] = exportFile(module.pkg, module.exportKey);
  return { agentDir, version, releaseDir, packages, direct };
}

function refusal(pkg, name, reason) {
  const fail = () => { throw rpcError(-32601, `unsupported_feature ${PACKAGES[pkg]}.${name}: ${reason}`); };
  return new Proxy(function refused() {}, { apply: fail, construct: fail,
    get(target, key) { if (typeof key === 'symbol' || key === 'then' || key === 'prototype') return Reflect.get(target, key); return fail(); } });
}

// Locates the install (failing loudly) and returns the overlay jiti aliases.
export function installedPiAliases(env = process.env) {
  const install = locateInstalledPi(env);
  const aliases = {};
  for (const prefix of PREFIXES) {
    for (const [specifier, module] of Object.entries(PI_MODULES)) {
      aliases[`${prefix}/${specifier}`] = module.shim ? overlayPath(module.shim) : install.direct[specifier];
    }
  }
  return { aliases, install };
}
// Installs the overlay registry. Shim namespaces are preloaded through the
// runtime's jiti so they are the same module instances path A would use; real
// Pi loads lazily, natively (outside jiti aliases), and only when imported.
export async function activateInstalledPi(jiti, install) {
  const shims = {};
  for (const key of Object.keys(PACKAGES)) shims[key] = await jiti.import(shimPath(key));
  const nativeRequire = createRequire(import.meta.url), cache = new Map();
  const namespace = key => {
    if (cache.has(key)) return cache.get(key);
    const pkg = install.packages[key], shim = shims[key];
    let real;
    try { real = nativeRequire(pkg.entry); } catch (error) { throw unavailable(`loading ${pkg.name}@${pkg.version} failed: ${String(error?.message || error).slice(0, 1024)}`); }
    const merged = { __esModule: true };
    for (const name of Object.keys(real)) {
      if (name === 'default') continue;
      if (REAL_OVERRIDES[key].has(name)) { merged[name] = real[name]; continue; }
      if (Object.hasOwn(shim, name)) continue;
      const reason = deniedReason[key].get(name);
      merged[name] = reason ? refusal(key, name, `${reason} (installed-Pi fallback)`)
        : allowed[key].has(name) ? real[name]
        : refusal(key, name, `not classified for the installed-Pi fallback in this octet build (Pi ${install.version})`);
    }
    for (const name of Object.keys(shim)) if (name !== 'default' && !(REAL_OVERRIDES[key].has(name) && Object.hasOwn(real, name))) merged[name] = shim[name];
    const view = new Proxy(merged, { get(target, name, receiver) {
      if (Reflect.has(target, name) || typeof name === 'symbol' || name === 'then' || name === 'toJSON' || name === 'default') return Reflect.get(target, name, receiver);
      throw rpcError(-32601, `unsupported_feature ${pkg.name}.${String(name)}: exported by neither installed Pi ${install.version} nor the octet shims`);
    } });
    cache.set(key, view); return view;
  };
  Object.defineProperty(globalThis, REGISTRY, { value: namespace, configurable: true, enumerable: false, writable: false });
}

// ---- path A failure classification ---------------------------------------
const SOURCE_EXTENSIONS = ['.ts', '.mts', '.cts', '.tsx', '.js', '.mjs', '.cjs', '.jsx'];
const MAX_FILES = 64, MAX_BYTES = 1048576, MAX_REPORTED = 32;
// An emulated module's named imports are checked against its shim; a module
// octet does not emulate (or an unmapped subpath) is reported as a whole.
const piPackage = specifier => {
  const pi = piModule(specifier);
  if (!pi) return null;
  return pi.shim && !pi.unmapped ? { key: pi.shim, subpath: false } : { key: null, subpath: true };
};
function resolveRelative(from, specifier) {
  const base = resolve(dirname(from), specifier);
  const stem = base.slice(0, base.length - extname(base).length);
  for (const candidate of [base, ...SOURCE_EXTENSIONS.map(e => base + e), ...SOURCE_EXTENSIONS.map(e => stem + e), ...SOURCE_EXTENSIONS.map(e => join(base, `index${e}`))]) {
    try { if (statSync(candidate).isFile()) return realpathSync(candidate); } catch {}
  }
  return null;
}
// Static scan of the entry and its relative-import closure for named imports
// of Pi packages that the shims do not export, or export only as path A
// refusals (REAL_OVERRIDES). Heuristic and bounded; it runs
// only after path A already failed, and only classifies that failure.
export async function scanPiImportGaps(entry, jiti) {
  const shimNames = {};
  for (const key of Object.keys(PACKAGES)) shimNames[key] = new Set(Object.keys(await jiti.import(shimPath(key))));
  const missing = [], subpaths = [], seen = new Set(), queue = [entry];
  while (queue.length && seen.size < MAX_FILES) {
    const file = queue.shift(); if (seen.has(file)) continue; seen.add(file);
    let text;
    try { if (statSync(file).size > MAX_BYTES) continue; text = readFileSync(file, 'utf8'); } catch { continue; }
    text = text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
    const specifiers = [];
    for (const m of text.matchAll(/\b(import|export)\s+(type\s+)?([\w$\s{},*]*?)\s*from\s*['"]([^'"\n]+)['"]/g)) {
      specifiers.push(m[4]);
      const pi = piPackage(m[4]);
      if (!pi || pi.subpath || m[2]) continue;
      const braces = /\{([^}]*)\}/.exec(m[3]);
      if (!braces) continue;
      for (let item of braces[1].split(',')) {
        item = item.trim(); if (!item || /^type\s/.test(item)) continue;
        const name = item.split(/\s+as\s+/)[0].trim();
        if (/^[\w$]+$/.test(name) && name !== 'default' && (!shimNames[pi.key].has(name) || REAL_OVERRIDES[pi.key].has(name))) missing.push({ specifier: m[4], name, file });
      }
    }
    for (const m of text.matchAll(/\b(?:import|require)\s*\(\s*['"]([^'"\n]+)['"]\s*\)|\bimport\s+['"]([^'"\n]+)['"]/g)) specifiers.push(m[1] ?? m[2]);
    for (const specifier of specifiers) {
      const pi = piPackage(specifier);
      if (pi?.subpath) subpaths.push({ specifier, file });
      if (specifier.startsWith('./') || specifier.startsWith('../')) { const next = resolveRelative(file, specifier); if (next && !seen.has(next)) queue.push(next); }
    }
  }
  const unique = list => [...new Map(list.map(x => [JSON.stringify(x), x])).values()].slice(0, MAX_REPORTED);
  return { missing: unique(missing), subpaths: unique(subpaths) };
}
// Returns the error to throw for a path A load failure: the original error
// unchanged unless the scan found Pi exports the shims lack.
export async function classifyLoadFailure(error, entry, jiti) {
  let gaps;
  try { gaps = await scanPiImportGaps(entry, jiti); } catch { return error; }
  if (!gaps.missing.length && !gaps.subpaths.some(s => fallbackSubpath(s.specifier))) return error;
  const names = [...gaps.missing.map(g => `${g.specifier}.${g.name}`), ...gaps.subpaths.filter(s => fallbackSubpath(s.specifier)).map(s => s.specifier)];
  const original = String(error?.message || error).slice(0, 1024);
  const classified = rpcError(FALLBACK_ELIGIBLE, `pi_compat_fallback_eligible ${entry}: octet shims lack or refuse ${names.join(', ').slice(0, 1024)}; the installed-Pi fallback requires explicit user approval (original error: ${original})`);
  classified.data = {
    fallback_eligible: true, entrypoint: entry,
    entrypoint_sha256: createHash('sha256').update(readFileSync(entry)).digest('hex'),
    missing_exports: gaps.missing, unsupported_subpaths: gaps.subpaths, original_error: original,
    supported_installed_pi: SUPPORTED_RANGE,
  };
  return classified;
}
export const fallbackData = error => error?.code === FALLBACK_ELIGIBLE && error.data?.fallback_eligible === true ? error.data : undefined;

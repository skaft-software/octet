import { createHash } from 'node:crypto';
import { constants, closeSync, existsSync, fstatSync, ftruncateSync, lstatSync, mkdirSync, openSync, opendirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { piThemeToNativeToml, readPiTheme } from './theme-palette.mjs';
import { colorToHex, parseColor } from './theme-colors.mjs';

const maxThemes = 64;
const thinkingTokens = { off: 'thinkingOff', minimal: 'thinkingMinimal', low: 'thinkingLow', medium: 'thinkingMedium', high: 'thinkingHigh', xhigh: 'thinkingXhigh', max: 'thinkingMax' };
const nativeColor = value => value === '' ? 'default' : typeof value === 'number' ? `index:${value}` : colorToHex(parseColor(value));
const selectorFor = name => /^[a-zA-Z0-9._-]+$/.test(name) ? `pi-${name}`
  : `pi-${name.replace(/[^a-zA-Z0-9._-]+/g, '-').slice(0, 48)}-${createHash('sha256').update(name).digest('hex').slice(0, 8)}`;
const shellQuote = value => `'${value.replaceAll("'", "'\\''")}'`;

// Stage only under the requested output. A symlink is not an overwrite target,
// even with explicit --overwrite. Never read or write ~/.octet/config.toml here.
export function checkThemeImportOutput(output, overwrite = false, themes = []) {
  for (const path of [output, join(output, 'themes')]) {
    let stat;
    try { stat = lstatSync(path); } catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`theme import directory must not be a symlink or file: ${path}`);
  }
  for (const path of [join(output, 'octet-config.toml'), ...themes.flatMap(theme => [theme.path, theme.nativePath])]) {
    let stat;
    try { stat = lstatSync(path); } catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`theme import output must be a regular non-symlink file: ${path}`);
    if (!overwrite) throw new Error(`${path.slice(output.length + 1)} exists; use --overwrite after reviewing the imported themes`);
  }
}

/** Data-only snapshot plan. Pi's resolver supplies enabled/trusted paths in its
 * precedence order; the first valid theme with a name wins, as in Pi 1.0.2.
 * No Pi resource callbacks or terminal queries run during registration capture.
 */
export function planThemeImport({ paths, selection, thinkingLevel = 'medium', output, builtinDir }) {
  output = resolve(output);
  if (!Array.isArray(paths) || paths.length > maxThemes) throw new Error(`theme import accepts at most ${maxThemes} paths`);
  const themes = [], diagnostics = [], names = new Set(), selectors = new Set();
  const sources = [...paths];
  if (builtinDir) for (const name of ['dark', 'light']) {
    const path = join(builtinDir, name + '.json');
    if (existsSync(path)) sources.push(path);
  }
  let inspected = 0;
  const load = path => {
    if (++inspected > maxThemes) throw new Error(`theme import accepts at most ${maxThemes} files`);
    try {
      const palette = readPiTheme(path), name = palette.name;
      if (!name || name === 'system') throw new Error('system/unnamed themes cannot be imported from a file');
      if (names.has(name)) { diagnostics.push(`theme collision ${name}: keeping the first palette; skipped ${path}`); return; }
      const selector = selectorFor(name);
      if (selectors.has(selector.toLowerCase())) throw new Error(`imported theme selector collision: ${selector}`);
      const base = piThemeToNativeToml(palette).toml;
      // Preserve the current Pi editor border, not the native model accent. This
      // is a snapshot of the configured thinking level; runtime level switching
      // and Pi's terminal-derived system palette remain host integration work.
      // The shared projection already owns every namespaced Pi role. Insert
      // the snapshot into its colors table, not into the last emitted role.
      const toml = base.replace('\n[colors]\n', '\n[colors]\n'
        + `composer_border = ${JSON.stringify(nativeColor(palette.colors[thinkingTokens[thinkingLevel] ?? 'thinkingOff']))}\n`);
      if (Buffer.byteLength(toml) > 262144) throw new Error('native theme exceeds 256 KiB');
      const imported = { name, selector, source: path, path: join(output, 'themes', selector + '.json'),
        nativePath: join(output, 'themes', selector + '.toml'), json: JSON.stringify(palette, null, 2) + '\n', toml };
      themes.push(imported); names.add(name); selectors.add(selector.toLowerCase());
    } catch (error) { diagnostics.push(`theme skipped ${path}: ${error.message}`); }
  };
  for (const path of sources) {
    try {
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) throw new Error('theme path must not be a symlink');
      if (stat.isDirectory()) {
        const directory = opendirSync(path), entries = [];
        try {
          let entry;
          while ((entry = directory.readSync())) {
            if (entries.length === 4096) throw new Error('theme directory exceeds 4096 entries');
            entries.push(entry);
          }
        } finally { directory.closeSync(); }
        for (const entry of entries.sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)) {
          if (entry.name.endsWith('.json')) load(join(path, entry.name));
        }
      } else load(path);
    } catch (error) { diagnostics.push(`theme skipped ${path}: ${error.message}`); }
  }
  const selected = themes.find(theme => theme.name === selection);
  if (!selected) diagnostics.push(!selection || selection === 'system'
    ? 'Pi system theme needs live terminal colors; no imported default was selected'
    : selection.includes('/') ? 'Pi automatic light/dark theme selection is not imported; select an imported palette explicitly'
      : `selected Pi theme ${selection} was not imported; no replacement default was selected`);
  // Use exact current files, not a directory glob: re-importing must not expose
  // stale palettes left in the staging directory by an earlier import.
  const launchArgs = themes.flatMap(theme => ['--theme-dir', theme.nativePath]);
  if (selected) launchArgs.push('--theme', selected.selector);
  const configPath = join(output, 'octet-config.toml');
  const config = '# Imported Pi appearance only. The enabled bridge contributes its session preference automatically.\n'
    + '# Optional standalone use without the bridge: these --theme-dir paths and --theme, or merge only the theme key after review.\n'
    + `# ${launchArgs.map(shellQuote).join(' ')}\n`
    + (selected ? `theme = ${JSON.stringify(selected.selector)}\n` : '# No imported default: see configure diagnostics.\n');
  return { output, themes, diagnostics, theme: selected?.selector, selected, thinkingLevel, configPath, config, launchArgs };
}

function write(path, source, overwrite) {
  const fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_NOFOLLOW | constants.O_NONBLOCK | (overwrite ? 0 : constants.O_EXCL), 0o600);
  try {
    if (!fstatSync(fd).isFile()) throw new Error(`theme import output must be a regular file: ${path}`);
    ftruncateSync(fd, 0); writeFileSync(fd, source);
  } finally { closeSync(fd); }
}

export function writeThemeImport(plan, { overwrite = false } = {}) {
  checkThemeImportOutput(plan.output, overwrite, plan.themes);
  mkdirSync(join(plan.output, 'themes'), { recursive: true, mode: 0o700 });
  for (const theme of plan.themes) { write(theme.path, theme.json, overwrite); write(theme.nativePath, theme.toml, overwrite); }
  write(plan.configPath, plan.config, overwrite);
}

export function themeImportLaunchHint(plan) {
  return plan.launchArgs.map((value, index) => index % 2 === 0 || value === plan.theme ? value : shellQuote(value)).join(' ');
}

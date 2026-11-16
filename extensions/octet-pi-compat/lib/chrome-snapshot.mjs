// Startup presentation crosses a process boundary: Node cannot paint Rust's
// first frame. Persist only declarative chrome, never opaque component text.
// Pi awaits input/before_agent_start before provider work (agent-session.ts:
// 2072,1956); this cache conveys presentation only, never hook readiness.
import { createHash } from 'node:crypto';
import { lstatSync, readFileSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { writePersisted } from './startup-cache.mjs';

export const CHROME_SCHEMA = 'octet.pi-chrome/v1';
const sgr = /\x1b\[[0-9;]*m/g;
const structural = /^[ ─━│┃┌┐└┘╭╮╰╯═║\uE000-\uF8FF·|/\\:,.%$()\[\]+\-]*$/u;
const vocabulary = /\b(?:ctx|context|tokens|total|sub|cost|off|minimal|low|medium|high|xhigh|max|auto|Pi)\b/g;
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
export const chromeSlot = (agent, workspace, trusted) => `chrome-${hash(`${agent}\n${workspace}\n${trusted}`)}.json`;

// Unknown text is not a literal: it could be a session title, a notification,
// or a credential printed by a factory. Decline rather than persist/redact it.
export function footerTemplate(lines, values) {
  const candidates = Object.entries(values).filter(([, value]) => typeof value === 'string' && value.length)
    .sort((a, b) => b[1].length - a[1].length);
  return lines.map(line => {
    const parts = [];
    let at = 0;
    while (at < line.length) {
      const rest = line.slice(at), style = /^\x1b\[[0-9;]*m/.exec(rest);
      if (style) { parts.push({text: style[0]}); at += style[0].length; continue; }
      const found = candidates.find(([, value]) => rest.startsWith(value));
      if (found) { parts.push({field: found[0]}); at += found[1].length; continue; }
      const word = /^(ctx|context|tokens|total|sub|cost|off|minimal|low|medium|high|xhigh|max|auto|Pi)\b/.exec(rest);
      if (word) { parts.push({text: word[0]}); at += word[0].length; continue; }
      // Numeric usage belongs to this session, not startup. Retain its shape
      // using zero fields, with no original digits in the persisted document.
      const number = /^\d+/.exec(rest);
      if (number) { parts.push({field: 'zero'}); at += number[0].length; continue; }
      const char = String.fromCodePoint(rest.codePointAt(0));
      if (!structural.test(char)) throw new Error('footer has non-template content');
      parts.push({text: char}); at += char.length;
    }
    // Coalesce literals so a wide blank footer stays small.
    return parts.reduce((out, part) => {
      if (part.text !== undefined && out.at(-1)?.text !== undefined) out.at(-1).text += part.text;
      else out.push(part);
      return out;
    }, []);
  });
}

export function editorTemplate(lines, columns, padding = 0, rows = 24) {
  if (lines.length < 3 || lines.length > 32) return undefined;
  const top = lines[0], bottom = lines.at(-1);
  if (![top, bottom].every(line => /^[ ─━│┃┌┐└┘╭╮╰╯═║]*$/u.test(line.replace(sgr, '')))) return undefined;

  padding = Math.min(padding, Math.floor((columns - 1) / 2));
  // Pi source: packages/tui/src/components/editor.ts:529-543 reserves the
  // cursor cell without padding and shows 30% of terminal rows (minimum five).
  return {top, bottom, padding, text_width: Math.max(1, columns - padding * 2 - (padding ? 0 : 1)),
    max_rows: Math.min(254, Math.max(5, Math.floor(rows * .3)))};
}

function fingerprint(path, directory = false) {
  try {
    const info = lstatSync(path);
    if (directory) {
      if (!info.isDirectory()) throw new Error('chrome directory changed');
      const names = readdirSync(path).sort();
      if (names.length > 4096) throw new Error('unbounded chrome directory');
      return hash(names.join('\n'));
    }
    if (!info.isFile() || info.size > 1024 * 1024) throw new Error('unbounded chrome identity');
    return hash(readFileSync(path));
  } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

export class ChromeSnapshot {
  constructor(runtime) { this.runtime = runtime; this.slots = new Map(); this.written = new Map(); }
  prepare(workspace, trusted, themePaths = []) {
    this.prepared = undefined;
    this.slots.clear();
    this.written.clear();
    const config = this.runtime.config;
    if (!config.mirror_pi_setup || !config.pi_agent_dir) return;
    try {
      const agent = resolve(config.pi_agent_dir);
      workspace = resolve(workspace);
      const paths = new Set([join(agent, 'settings.json'), join(agent, 'keybindings.json'),
        join(agent, 'npm/package-lock.json'), join(agent, 'install/current-version')]);
      if (trusted) { paths.add(join(workspace, '.pi/settings.json')); paths.add(join(workspace, '.pi/keybindings.json')); }
      const directories = new Set([join(agent, 'extensions'), join(agent, 'themes')]);
      if (trusted) { directories.add(join(workspace, '.pi/extensions')); directories.add(join(workspace, '.pi/themes')); }
      for (const file of config.extensions ?? []) {
        paths.add(file); directories.add(dirname(file));
        for (const source of Object.keys(config.entrypoint_sources?.[file] ?? {})) { paths.add(source); directories.add(dirname(source)); }
        // Include package versions even if their lockfile is absent.
        for (let dir = dirname(file); dir !== dirname(dir); dir = dirname(dir)) {
          const packageFile = join(dir, 'package.json');
          if (fingerprint(packageFile) !== null) { paths.add(packageFile); break; }
          if (dir === agent || dir === workspace || (!dir.startsWith(agent + '/') && !dir.startsWith(workspace + '/'))) break;
        }
      }
      for (const file of [...(config.pi_theme_paths ?? []), ...themePaths]) paths.add(file);
      if (paths.size > 512) return;
      const identity = [...paths].sort().map(path => ({path, hash: fingerprint(path)})).concat(
        [...directories].sort().map(path => ({path, hash: fingerprint(path, true), directory: true})));
      if (identity.length > 512) return;
      this.prepared = {agent, workspace, trusted, identity};
    } catch { /* Unsupported identity inputs mean no startup cache. */ }
  }
  capture(surface, lines) {
    if (!this.prepared || !['editor', 'footer'].includes(surface.placement)) return;
    try {
      const state = surface.store.state, {agent, workspace, trusted} = this.prepared;
      if (workspace !== resolve(state.workspace) || trusted !== (state.host.project_trusted === true)) return;
      const slot = chromeSlot(agent, workspace, trusted);
      let entry = this.slots.get(slot);
      if (!entry || entry.columns !== surface.columns || entry.rows !== surface.rows) {
        entry = {schema: CHROME_SCHEMA, ...this.prepared, columns: surface.columns, rows: surface.rows};
        this.slots.set(slot, entry);
      }
      const model = state.host.model_view;
      entry.defaults = Object.fromEntries(Object.entries({native_model: state.host.model, model: model?.id,
        model_name: model?.name, provider: model?.provider, reasoning: state.host.reasoning})
        .filter(([, value]) => typeof value === 'string'));
      if (surface.placement === 'editor') {
        const facade = [...(surface.nativeEditorObjects ?? [])][0];
        entry.editor = facade && lines.length === facade.startupRows + 2
          ? editorTemplate(lines, surface.columns, facade.startupPadding, surface.rows) : undefined;
      } else {
        entry.footer = footerTemplate(lines, {
          model: model?.id ?? model?.model_id, model_name: model?.name,
          provider: model?.provider, reasoning: state.host.reasoning,
          workspace, home_workspace: workspace.replace(homedir(), '~'),
        });
      }
      if (entry.editor || entry.footer) {
        const text = JSON.stringify(entry);
        if (this.written.get(slot) === text) return;
        // Bind the cache to the inputs observed before factories executed. A
        // concurrent settings/source/version edit must never bless old chrome.
        if (!entry.identity.every(item => fingerprint(item.path, item.directory) === item.hash)) return;
        if (writePersisted(slot, entry, 131072)) this.written.set(slot, text);
      }
    } catch { /* Optional, data-only cache: unsupported chrome keeps native fallback. */ }
  }
}

// Host validation mirrors this grammar; no executable templates or raw text.
export function safeChromeLiteral(text) {
  return typeof text === 'string' && text.length <= 16384 && structural.test(text.replace(sgr, '').replace(vocabulary, ''));
}

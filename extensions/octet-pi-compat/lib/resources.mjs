import { homedir } from 'node:os';
import { basename, extname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { bounded, facade, invalid, ownerKey, rpcError, unsupported } from './errors.mjs';
import { createContext } from './api.mjs';
import { discoverPiSetup, MAX_RESOURCE_PATHS, MAX_RESOURCE_PATH_BYTES_PER_PATH, MAX_RESOURCE_PATH_BYTES, resolveThemeFile, themeCandidates } from './pi-setup.mjs';

const pathFields = { skillPaths: 'skill_paths', promptPaths: 'prompt_paths', themePaths: 'theme_paths' };
function record(value, allowed, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) invalid(`${label} must be a plain object`);
  for (const key of Reflect.ownKeys(value)) if (!allowed.includes(key)) unsupported(`${label}.${String(key)}`, 'field would not be honored');
}
function pathText(value, label, max = MAX_RESOURCE_PATH_BYTES_PER_PATH) {
  bounded(value, label, max, { controls: true });
  if (/[\x00-\x1f\x7f-\x9f]/u.test(value)) invalid(`${label} contains controls`);
  return value;
}

// Path normalization adapted from Pi 1.0.2 (cd32f7725fdbddbaecdff5b1e68491563394e0ca),
// packages/coding-agent/src/utils/paths.ts and core/resource-loader.ts.
// Copyright (c) 2025 Mario Zechner. MIT, see ../LICENSE.pi.
// Lexical only: do not follow symlinks or assume host filesystem admission.
function normalizePath(value) {
  if (process.platform === 'win32' && value.startsWith('/') && !value.startsWith('//') && !value.includes('\\')) {
    const match = value.match(/^\/(?:mnt\/|cygdrive\/)?([a-z])(?:\/(.*))?$/i);
    if (match) value = `${match[1].toUpperCase()}:\\${match[2]?.replaceAll('/', '\\') ?? ''}`;
  }
  if (value === '~') return homedir();
  if (value.startsWith('~/') || process.platform === 'win32' && value.startsWith('~\\')) return join(homedir(), value.slice(2));
  return /^file:\/\//.test(value) ? fileURLToPath(value) : value;
}
export function normalizeResourcePath(value, cwd) {
  bounded(value, 'resource path input', MAX_RESOURCE_PATH_BYTES_PER_PATH, { controls: true });
  value = pathText(value.trim(), 'resource path');
  if (value.startsWith('builtin:') || value.startsWith('<')) unsupported('synthetic resource path', 'native discovery accepts filesystem paths only');
  return pathText(resolve(normalizePath(cwd), normalizePath(value)), 'normalized resource path');
}

function appendPaths(paths, contribution, budget) {
  if (contribution === undefined) return;
  record(contribution, Object.keys(pathFields), 'resources_discover result');
  for (const [field, wire] of Object.entries(pathFields)) {
    if (!Object.hasOwn(contribution, field)) continue;
    const values = contribution[field];
    if (values === undefined) continue;
    if (!Array.isArray(values) || Object.getPrototypeOf(values) !== Array.prototype) invalid(`${field} must be a string array`);
    if (budget.count + values.length > MAX_RESOURCE_PATHS) invalid('bounds_exceeded resource path count');
    for (const key of Reflect.ownKeys(values)) {
      if (key !== 'length' && (typeof key !== 'string' || !/^(0|[1-9][0-9]*)$/.test(key) || Number(key) >= values.length)) unsupported(`${field}.${String(key)}`, 'array field would not be honored');
    }
    for (let index = 0; index < values.length; index++) {
      if (!Object.hasOwn(values, index)) invalid(`${field} must not contain holes`);
      const path = normalizeResourcePath(values[index], budget.cwd);
      budget.count++; budget.bytes += Buffer.byteLength(path);
      if (budget.bytes > MAX_RESOURCE_PATH_BYTES) invalid('bounds_exceeded resource path bytes');
      paths[wire].push(path); // Preserve duplicates and order; native loaders own precedence.
    }
  }
}

// Every enabled bridge contributes the current enabled Pi palettes. Only mirror
// mode contributes the other setup resources/preferences. Discovery reads data,
// never factories, and native admission still owns trust and publication.
function appendPiSetupPaths(runtime, paths, budget) {
  const setup = discoverPiSetup({ agentDir: runtime.config.pi_agent_dir, env: process.env,
    cwd: budget.cwd, projectTrusted: runtime.host.project_trusted === true });
  if (setup.resourceLimitsExceeded.length) invalid(`bounds_exceeded Pi setup resources: ${setup.resourceLimitsExceeded.map(item => `${item.kind} ${item.actual}/${item.limit} ${item.dimension}`).join(', ')}`);
  // Keep exact regular files, including malformed JSON. Native discovery
  // resolves later-root winners before validation: dropping a broken winning
  // file here could incorrectly resurrect a shadowed palette.
  const themeFiles = themeCandidates(setup.themesPaths, setup.diagnostics);
  // Validate immutable import declarations before accounting the complete
  // palette contribution. Snapshots follow live paths for native precedence.
  const snapshots = { skill_paths: [], prompt_paths: [], theme_paths: [] };
  appendPaths(snapshots, { themePaths: runtime.config.pi_theme_paths }, { ...budget });
  let preferred;
  if (runtime.config.pi_theme?.native_path !== undefined) {
    preferred = normalizeResourcePath(runtime.config.pi_theme.native_path, budget.cwd);
    if (!snapshots.theme_paths.includes(preferred)) invalid('imported default theme must be a contributed theme path');
  } else if (runtime.mirrorPiSetup) {
    preferred = resolveThemeFile(themeFiles, setup.defaultTheme, setup.diagnostics);
    if (!preferred && setup.defaultTheme) setup.diagnostics.push(`theme ${setup.defaultTheme} was not found in the Pi setup; no native preference`);
  }
  const admit = (values, label) => {
    const normalized = values.map(value => normalizeResourcePath(value, budget.cwd));
    const bytes = normalized.reduce((sum, path) => sum + Buffer.byteLength(path), 0);
    if (budget.count + normalized.length > MAX_RESOURCE_PATHS) invalid(`bounds_exceeded ${label} exceeds the shared ${MAX_RESOURCE_PATHS}-path resource budget`);
    if (budget.bytes + bytes > MAX_RESOURCE_PATH_BYTES) invalid(`bounds_exceeded ${label} exceeds the shared ${MAX_RESOURCE_PATH_BYTES}-byte resource budget`);
    budget.count += normalized.length; budget.bytes += bytes;
    return normalized;
  };
  // Session-only mirror paths also share the native budget. Reserve these few
  // paths first, then palettes before potentially large skill/prompt inventories.
  if (runtime.mirrorPiSetup) {
    if (setup.keybindingsPath) paths.keybindings_paths = admit([setup.keybindingsPath], 'keybindings');
    if (setup.contextPath) paths.context_paths = admit([setup.contextPath], 'context');
    if (setup.defaultModel) paths.default_model = setup.defaultModel;
    if (setup.defaultThinkingLevel) paths.default_thinking_level = setup.defaultThinkingLevel;
  }
  const themeContributions = [...themeFiles, ...snapshots.theme_paths];
  const ordered = [...new Set(themeContributions)];
  // Charge the raw resource contributions before de-duplicating output paths.
  // Discovery fails closed rather than silently selecting a fitting subset.
  const admitted = new Set(admit(themeContributions, 'Pi themes'));
  const stem = path => basename(path, extname(path));
  const winners = new Map(ordered.map(path => [stem(path), path]));
  paths.theme_paths.push(...ordered.filter(path => admitted.has(path) && admitted.has(winners.get(stem(path)))));
  if (preferred && admitted.has(preferred)) paths.default_theme = preferred;
  if (runtime.mirrorPiSetup) {
    paths.skill_paths.push(...admit(setup.skillsPaths, 'Pi skills'));
    paths.prompt_paths.push(...admit(setup.promptsPaths, 'Pi prompts'));
  }
  for (const diagnostic of setup.diagnostics.slice(0, 32)) runtime.diagnostic(`Pi setup ${diagnostic}`);
  if (setup.diagnostics.length > 32) runtime.diagnostic(`Pi setup: ${setup.diagnostics.length - 32} further discovery diagnostics omitted`);
}

async function cancellable(promise, signal) {
  signal.throwIfAborted();
  let abort;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      abort = () => reject(signal.reason);
      signal.addEventListener('abort', abort, { once: true });
    })]);
  } finally { signal.removeEventListener('abort', abort); }
}

export async function discoverResources(runtime, params, store) {
  runtime.require('resource_paths_v1');
  if (!runtime.metadata().hooks.includes('resources_discover')) invalid('unknown hook resources_discover');
  record(params.payload, ['cwd', 'reason'], 'resources_discover payload');
  const { cwd, reason } = params.payload;
  pathText(cwd, 'resources_discover cwd');
  if (!isAbsolute(cwd)) invalid('resources_discover cwd must be absolute');
  if (!['startup', 'reload'].includes(reason)) invalid('resources_discover reason');
  // This specialized hook accepts only the native context owner, never payload.binding.
  ownerKey(params.context?.resource_owner);
  runtime.bind(params, store);
  store.resourceDiscovery = true;
  const signal = store.controller.signal;
  const live = () => { signal.throwIfAborted(); runtime.assertSessionOwner(store); };
  return cancellable(runtime.queued(store, async () => {
    live();
    const paths = { skill_paths: [], prompt_paths: [], theme_paths: [] }, budget = { cwd, count: 0, bytes: 0 };
    appendPiSetupPaths(runtime, paths, budget);
    // Snapshot before the first callback, in factory/registration order, like Pi.
    for (const entry of [...runtime.events.get('resources_discover') || []]) {
      live();
      const child = { ...store, factory: entry.factory };
      const provenance = `resources_discover factory ${runtime.entrypoint(entry.factory)}`;
      let contribution;
      try {
        contribution = await cancellable(runtime.scope.run(child, () => entry.handler(
          facade({ type: 'resources_discover', cwd, reason }, 'resources_discover event'), createContext(runtime, child))), signal);
      } catch (error) {
        live();
        // Ordinary callback failures are diagnosed and later handlers still run,
        // as in Pi. Adapter/native contract refusals must not become false success.
        if (Number.isInteger(error?.code)) throw error;
        runtime.reportCallbackError('resources_discover', entry.factory, error);
        continue;
      }
      live();
      try { appendPaths(paths, contribution, budget); }
      catch (error) { throw rpcError(error.code ?? -32602, `${provenance}: ${error.message}`); }
    }
    await cancellable(runtime.flush(store), signal);
    live();
    return { resource_paths: paths };
  }), signal);
}

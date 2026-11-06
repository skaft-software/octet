import { join, resolve } from 'node:path';
import { bounded, fields, invalid, plainJSON, rpcError, unsupported } from './errors.mjs';
import { getAgentDir } from './public-helpers.mjs';
import { captureSettingsHost } from './children.mjs';

const SECRET = Symbol('native settings manager');
function live({ runtime, store }) {
  runtime.require('pi_settings_v1');
  runtime.assertOwner(store);
  store.controller.signal.throwIfAborted();
  if (runtime.stopping) throw rpcError(-32002, 'host is draining');
}
function object(value, label) {
  fields(value, Object.keys(value ?? {}), label);
  return plainJSON(value, label, 524288);
}
function path(value, label) {
  bounded(value, label, 4096);
  return resolve(value);
}
function reply(value, expected) {
  fields(value, ['global', 'project', 'project_trusted', 'global_path', 'project_path', 'errors'], 'Pi settings reply');
  if (value.project_trusted !== expected.project_trusted || value.global_path !== expected.global_path || value.project_path !== expected.project_path) invalid('Pi settings binding');
  if (!Array.isArray(value.errors) || value.errors.length > 2) invalid('Pi settings errors');
  const errors = value.errors.map(error => {
    fields(error, ['scope', 'path', 'message'], 'Pi settings error');
    if (!['global', 'project'].includes(error.scope) || error.path !== expected[`${error.scope}_path`]) invalid('Pi settings error scope');
    if (error.scope === 'project' && !expected.project_trusted) invalid('untrusted project settings error');
    return { scope: error.scope, path: error.path, error: new Error(bounded(error.message, 'Pi settings error', 4096, { controls: true })) };
  });
  const global = object(value.global, 'global Pi settings'), project = object(value.project, 'project Pi settings');
  if (!expected.project_trusted && Object.keys(project).length) invalid('untrusted project settings');
  return { global, project, errors };
}

export class SettingsManager {
  #binding;
  #params;
  #global;
  #errors;
  #globalLoadError;
  #writeQueue = Promise.resolve();
  #globalPath;

  constructor(secret, binding, params, loaded) {
    if (secret !== SECRET) unsupported('SettingsManager', 'use the native-mediated SettingsManager.create');
    this.#binding = binding;
    this.#params = params;
    this.#global = loaded.global;
    this.#errors = loaded.errors;
    this.#globalLoadError = loaded.errors.some(error => error.scope === 'global');
    this.#globalPath = join(params.agent_dir, 'settings.json');
  }
  static create(cwd, agentDir = getAgentDir(), options = {}) {
    fields(options, ['projectTrusted'], 'SettingsManager.create');
    const binding = captureSettingsHost();
    live(binding);
    const { runtime, store } = binding;
    const directory = path(agentDir, 'Pi agent directory'), workspace = path(cwd, 'Pi settings workspace');
    if (directory !== resolve(runtime.config.pi_agent_dir ?? getAgentDir()) || workspace !== resolve(store.state.workspace)) invalid('Pi settings path must match the native configured directory and workspace');
    const trusted = options.projectTrusted ?? true;
    if (typeof trusted !== 'boolean' || trusted && store.state.host.project_trusted !== true) unsupported('SettingsManager project trust', 'native workspace trust is required');
    const params = { cwd: workspace, agent_dir: directory, project_trusted: trusted };
    const value = runtime.transport.requestSync('pi/settings/read', {
      parent_request_id: store.id, resource_owner: store.state.owner, ...params,
    }, { parent: store.live ? store.id : undefined, signal: store.controller.signal });
    const loaded = reply(value, { project_trusted: trusted, global_path: join(directory, 'settings.json'), project_path: join(workspace, '.pi', 'settings.json') });
    return new SettingsManager(SECRET, binding, params, loaded);
  }
  static inMemory() { unsupported('SettingsManager.inMemory', 'native child settings inherit from the owning host'); }
  getGlobalSettings() { live(this.#binding); return structuredClone(this.#global); }
  setExtensionPaths(paths) {
    live(this.#binding);
    if (!Array.isArray(paths) || paths.length > 1024) invalid('Pi extension paths');
    const extensions = paths.map(value => bounded(value, 'Pi extension path', 4096));
    plainJSON(extensions, 'Pi extension paths', 524288);
    this.#global.extensions = extensions;
    if (this.#globalLoadError) return;
    this.#writeQueue = this.#writeQueue.then(async () => {
      live(this.#binding);
      const { runtime, store } = this.#binding;
      const result = await runtime.hostCall('pi/settings/update', { ...this.#params, scope: 'global', extensions }, store);
      fields(result, [], 'Pi settings commit');
    }).catch(error => {
      this.#errors.push({ scope: 'global', path: this.#globalPath, error: error instanceof Error ? error : new Error(String(error)) });
    });
  }
  async flush() { await this.#writeQueue; }
  drainErrors() { const errors = this.#errors; this.#errors = []; return errors; }
}

// Command-context replacements keep the command RPC live, not its old Session.
import { bounded, fields, invalid, unsupported } from './errors.mjs';

export function sessionMethods(runtime, store, createContext) {
  const command = name => {
    if (store.method !== 'command/execute') unsupported(`ctx.${name}`, 'command context only');
    runtime.assertOwner(store);
  };
  const replace = async (name, method, params, withSession) => {
    command(name);
    if (withSession !== undefined && typeof withSession !== 'function') invalid(`${name} withSession`);
    runtime.require('session_control_v1');
    const result = await runtime.track(runtime.hostCall(method, {
      resource_owner: store.state.owner, ...params,
    }, store), store);
    fields(result, ['session_id', 'cancelled'], `${name} receipt`);
    if (result.cancelled !== undefined && typeof result.cancelled !== 'boolean') invalid(`${name} cancelled`);
    if (result.cancelled) return { cancelled: true };
    bounded(result.session_id, 'replacement session id', 256);
    // Rebinding is a host lifecycle receipt. Never retarget captured old ctxs.
    const state = await runtime.foregroundFor(result.session_id, store.controller.signal);
    if (withSession) {
      const fresh = { ...store, state };
      await runtime.scope.run(fresh, () => withSession(createContext(runtime, fresh, true)));
    }
    return { cancelled: false };
  };
  return {
    newSession(options = {}) {
      fields(options, ['withSession'], 'newSession options');
      return replace('newSession', 'session/create', {}, options.withSession);
    },
    fork(entryId, options = {}) {
      fields(options, ['position', 'withSession'], 'fork options'); bounded(entryId, 'entry id', 256);
      if (options.position !== undefined && !['before', 'at'].includes(options.position)) invalid('fork position');
      return replace('fork', 'session/fork', { entry_id: entryId, ...(options.position ? { position: options.position } : {}) }, options.withSession);
    },
    switchSession(sessionPath, options = {}) {
      fields(options, ['withSession'], 'switchSession options'); bounded(sessionPath, 'session path', 4096);
      return replace('switchSession', 'session/switch', { session_id: sessionPath.split(/[\\/]/).pop().replace(/\.[^.]*$/, '') }, options.withSession);
    },
    reload() {
      command('reload'); runtime.require('session_control_v1');
      return runtime.track(runtime.hostCall('session/reload', { resource_owner: store.state.owner }, store), store).then(() => undefined);
    },
  };
}

// Pi 1.0.2 context facts and raw-input transforms. Rust supplies native facts;
// Pi's no-project-resources trust signal does not change native resource admission.
// Extension request cancellation is not a substitute for aborting the agent.
import { bounded, facade, fields, invalid, plainJSON, rpcError, strict, unsupported, unsupportedAPI } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';
import { HookBudget } from './handler-budget.mjs';
import { inputImages, visibleImages } from './image-content.mjs';
import { readPiSettings } from './pi-setup.mjs';
import { retainPiProjectTrust } from './project-trust.mjs';

function fact(runtime, store, key, api) {
  runtime.assertSessionOwner(store);
  if (!Object.hasOwn(store.state.host, key)) unsupportedAPI(api, `authoritative ${key} snapshot required`);
  return store.state.host[key];
}
export function contextFacts(runtime, store) {
  return {
    get mode() {
      const mode = fact(runtime, store, 'mode', 'ctx.mode');
      if (!['tui', 'rpc', 'json', 'print'].includes(mode)) invalid('host extension mode');
      return mode;
    },
    isProjectTrusted() {
      const trusted = fact(runtime, store, 'project_trusted', 'ctx.isProjectTrusted');
      if (typeof trusted !== 'boolean') invalid('host project trust');
      retainPiProjectTrust(store.state);
      return trusted || store.state.piPlainWorkspaceTrusted;
    },
    getSystemPrompt() {
      runtime.require('system_prompt_read');
      if (runtime.stopping) throw rpcError(-32002, 'host is draining');
      runtime.assertOwner(store);
      store.controller.signal.throwIfAborted();
      const scoped = store.promptReadScope !== undefined && runtime.features.has('run_system_prompt_read_v1');
      const observation = !store.live && store.method === 'turn/started' && scoped
        && store.promptReadScope === store.state.promptReadScope && store.state.runActive;
      if (!observation && (!Number.isSafeInteger(store.id) || store.id < 0 || !store.live
          || runtime.active.get(store.id)?.controller !== store.controller)) {
        throw rpcError(-32002, 'not_foreground_owner system prompt read requires a live request');
      }
      const parent = observation ? 0 : store.id;
      const result = runtime.transport.requestSync('context/system_prompt', {
        parent_request_id: parent, resource_owner: store.state.owner,
        ...(scoped ? { scope: store.promptReadScope } : {}),
      }, { parent, signal: store.controller.signal });
      fields(result, ['text'], 'system prompt reply');
      const text = bounded(result.text, 'system prompt text', 262144, { controls: true });
      if (text.includes('\0')) invalid('system prompt text must not contain NUL');
      return text;
    },
    getSystemPromptOptions() {
      if (store.method !== 'command/execute') unsupported('ctx.getSystemPromptOptions', 'command context only');
      return plainJSON(fact(runtime, store, 'system_prompt_options', 'ctx.getSystemPromptOptions'), 'system prompt options', 524288);
    },
  };
}
export function getSettings(runtime, store) {
  runtime.assertSessionOwner(store);
  if (Object.hasOwn(store.state.host, 'settings')) return plainJSON(store.state.host.settings, 'settings', 524288);
  // The user's own Pi settings, read the way Pi reads them. Project settings
  // count only under the host's trust decision, never a Pi trust file.
  const { settings } = readPiSettings({ agentDir: runtime.config.pi_agent_dir, cwd: store.state.workspace, projectTrusted: store.state.host.project_trusted === true });
  return plainJSON(settings, 'settings', 524288);
}

// An independent early phase of before_prompt. Never invokes before_agent_start;
// handled input must not reach prompt expansion, persistence or a provider.
export async function transformInput(runtime, payload, store) {
  fields(payload, ['phase', 'text', 'images', 'source', 'streaming_behavior'], 'native input payload');
  if (payload.phase !== 'input' || !['interactive', 'rpc', 'extension'].includes(payload.source)) invalid('native input phase/source');
  if (payload.streaming_behavior != null && !['steer', 'followUp'].includes(payload.streaming_behavior)) invalid('input streaming behavior');
  // One snapshot for this dispatch: unsubscribe/register affects the next input.
  const handlers = [...runtime.events.get('input') || []];
  if (!handlers.length) return { action: 'continue' };
  let text = bounded(payload.text, 'input text', 262144, { controls: true });
  let images = visibleImages(payload.images);
  const originalText = text, originalImages = JSON.stringify(images), signal = store.controller.signal;
  const budget = new HookBudget(runtime, store, 'input');
  try {
    for (const entry of handlers) {
      signal.throwIfAborted(); runtime.assertSessionOwner(store);
      const child = { ...store, factory: entry.factory };
      let result;
      try {
        const run = await cancellable(budget.run(entry, child, () => runtime.scope.run(child, () => entry.handler(facade({
          type: 'input', text, images, source: payload.source, streamingBehavior: payload.streaming_behavior ?? undefined,
        }, 'input event'), createContext(runtime, child)))), signal);
        if (!run.ran) continue; // a stalled input handler must not swallow the prompt
        result = run.value;
      } catch (error) {
        signal.throwIfAborted(); runtime.assertSessionOwner(store);
        if (Number.isInteger(error?.code)) throw error;
        runtime.reportCallbackError('input', entry.factory, error);
        continue;
      }
      signal.throwIfAborted(); runtime.assertSessionOwner(store);
      if (result === undefined) continue;
      fields(result, ['action', 'text', 'images'], 'input result');
      if (result.action === 'handled') {
        await cancellable(runtime.flush(store), signal);
        return { action: 'handled' };
      }
      if (result.action === 'continue') continue;
      if (result.action !== 'transform') invalid('input action');
      text = bounded(result.text, 'transformed input text', 262144, { controls: true });
      // Pi's nullish fallback retains attachments; [] explicitly clears them.
      if (result.images != null) { inputImages(result.images); images = result.images; }
    }
  } finally { budget.finish(); }
  await cancellable(runtime.flush(store), signal);
  signal.throwIfAborted(); runtime.assertSessionOwner(store);
  return text !== originalText || JSON.stringify(images) !== originalImages
    ? { action: 'transform', text, ...(images === undefined ? {} : { images: inputImages(images) }) }
    : { action: 'continue' };
}

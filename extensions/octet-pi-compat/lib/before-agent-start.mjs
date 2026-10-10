import { bounded, facade, fields, invalid } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';
import { customMessage } from './custom-messages.mjs';
import { performance } from 'node:perf_hooks';

function system(text) {
  bounded(text, 'before_agent_start systemPrompt', 262144, { controls: true });
  if (text.includes('\0')) invalid('before_agent_start systemPrompt contains NUL');
  return text;
}

// Native BeforePrompt owns applying this per-run replacement. It is never an
// additive context fragment, persisted Session edit or provider payload override.
export async function beforeAgentStart(runtime, payload, store) {
  runtime.require('before_prompt_state_v1'); runtime.assertSessionOwner(store);
  fields(payload, ['prompt', 'system_prompt'], 'before_agent_start native payload');
  bounded(payload.prompt, 'before_agent_start prompt', 262144, { controls: true });
  let effective = system(payload.system_prompt);
  const messages = [];
  store.providerContext = true;
  for (const entry of [...runtime.events.get('before_agent_start') || []]) {
    store.controller.signal.throwIfAborted(); runtime.assertSessionOwner(store);
    const child = { ...store, factory: entry.factory };
    const event = facade({ type: 'before_agent_start', prompt: payload.prompt, systemPrompt: effective, images: undefined }, 'before_agent_start event');
    let result;
    const trace = process.env.OCTET_PI_TRACE_HOOKS === '1';
    const entrypoint = runtime.config.extensions[entry.factory];
    const started = performance.now();
    if (trace) console.error(`[pi-compat hook] before_agent_start begin ${entrypoint}`);
    try {
      result = await cancellable(runtime.scope.run(child, () => entry.handler(event, createContext(runtime, child))), store.controller.signal);
    } catch (error) {
      if (error?.code || store.controller.signal.aborted) throw error;
      runtime.reportCallbackError('before_agent_start', entry.factory, error);
      continue;
    } finally {
      if (trace) console.error(`[pi-compat hook] before_agent_start end ${entrypoint} ${(performance.now() - started).toFixed(1)}ms`);
    }
    store.controller.signal.throwIfAborted(); runtime.assertSessionOwner(store);
    if (result !== undefined) {
      fields(result, ['systemPrompt', 'message'], 'before_agent_start result');
      if (result.message !== undefined) messages.push(customMessage(result.message));
      if (result.systemPrompt !== undefined) effective = system(result.systemPrompt);
    }
    await runtime.flush(child);
  }
  return { systemPrompt: effective, messages };
}

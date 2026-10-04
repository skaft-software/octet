import { bounded, fields, invalid, strict } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';

function system(text) {
  bounded(text, 'before_agent_start systemPrompt', 262144, { controls: true });
  if (text.includes('\0')) invalid('before_agent_start systemPrompt contains NUL');
  return text;
}

// Native BeforePrompt owns applying this per-run replacement. It is never an
// additive context fragment, persisted Session edit or provider payload override.
export async function beforeAgentStart(runtime, payload, store) {
  runtime.require('before_prompt_state_v1'); runtime.assertOwner(store);
  fields(payload, ['prompt', 'system_prompt'], 'before_agent_start native payload');
  bounded(payload.prompt, 'before_agent_start prompt', 262144, { controls: true });
  let effective = system(payload.system_prompt);
  store.providerContext = true;
  for (const entry of runtime.events.get('before_agent_start') || []) {
    store.controller.signal.throwIfAborted(); runtime.assertOwner(store);
    const child = { ...store, factory: entry.factory };
    const event = strict({ type: 'before_agent_start', prompt: payload.prompt, systemPrompt: effective, images: undefined }, 'before_agent_start event');
    let result;
    try {
      result = await cancellable(runtime.scope.run(child, () => entry.handler(event, createContext(runtime, child))), store.controller.signal);
    } catch (error) {
      if (error?.code || store.controller.signal.aborted) throw error;
      runtime.backgroundError(new Error('before_agent_start callback failed (private prompt details redacted)'));
      continue;
    }
    store.controller.signal.throwIfAborted(); runtime.assertOwner(store);
    if (result !== undefined) {
      fields(result, ['systemPrompt'], 'before_agent_start result');
      if (result.systemPrompt !== undefined) effective = system(result.systemPrompt);
    }
    await runtime.flush(child);
  }
  return effective;
}

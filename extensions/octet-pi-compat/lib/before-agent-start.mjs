import { bounded, facade, fields, invalid } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';
import { customMessage } from './custom-messages.mjs';
import { HookBudget } from './handler-budget.mjs';

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
  // Every handler shares the host's 5 s before_prompt deadline: a stalled one is
  // detached and skipped (reported once) instead of failing the whole prompt.
  const budget = new HookBudget(runtime, store, 'before_agent_start');
  try {
    for (const entry of [...runtime.events.get('before_agent_start') || []]) {
      store.controller.signal.throwIfAborted(); runtime.assertSessionOwner(store);
      const child = { ...store, factory: entry.factory };
      // Pi 1.2 tags user-prompt runs; the host runs this hook for those only.
      const event = facade({ type: 'before_agent_start', messageType: 'user', prompt: payload.prompt, systemPrompt: effective, images: undefined }, 'before_agent_start event');
      let result;
      try {
        const run = await cancellable(budget.run(entry, child, () => runtime.scope.run(child, () => entry.handler(event, createContext(runtime, child)))), store.controller.signal);
        if (!run.ran) continue;
        result = run.value;
      } catch (error) {
        if (error?.code || store.controller.signal.aborted) throw error;
        runtime.reportCallbackError('before_agent_start', entry.factory, error);
        continue;
      }
      store.controller.signal.throwIfAborted(); runtime.assertSessionOwner(store);
      if (result !== undefined) {
        fields(result, ['systemPrompt', 'message'], 'before_agent_start result');
        if (result.message !== undefined) messages.push(customMessage(result.message));
        if (result.systemPrompt !== undefined) effective = system(result.systemPrompt);
      }
      await runtime.flush(child);
    }
  } finally { budget.finish(); }
  return { systemPrompt: effective, messages };
}

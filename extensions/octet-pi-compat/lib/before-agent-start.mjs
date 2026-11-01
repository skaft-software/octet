import { bounded, facade, fields, invalid } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';
import { customMessage } from './custom-messages.mjs';
import { HookBudget } from './handler-budget.mjs';
import { promptOptions } from './system-prompt-options.mjs';

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
  const handlers = [...runtime.events.get('before_agent_start') || []];
  const prompt = promptOptions(runtime, store, system(payload.system_prompt), handlers[0]?.factory);
  const messages = [];
  store.providerContext = true;
  // Every handler shares the host's 5 s before_prompt deadline: a stalled one is
  // detached and skipped (reported once) instead of failing the whole prompt.
  const budget = new HookBudget(runtime, store, 'before_agent_start');
  try {
    for (const entry of handlers) {
      store.controller.signal.throwIfAborted(); runtime.assertSessionOwner(store);
      const child = { ...store, factory: entry.factory };
      const event = facade({ type: 'before_agent_start', messageType: 'user', prompt: payload.prompt,
        get systemPrompt() { return system(prompt.render()); }, systemPromptOptions: prompt.options, images: undefined }, 'before_agent_start event');
      const context = createContext(runtime, child);
      Object.defineProperty(context, 'getSystemPrompt', { value: () => { runtime.assertSessionOwner(child); child.controller.signal.throwIfAborted(); return system(prompt.render()); } });
      let result;
      try {
        const run = await cancellable(budget.run(entry, child, () => runtime.scope.run(child, () => entry.handler(event, context))), store.controller.signal);
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
        if (result.systemPrompt !== undefined) prompt.options.forceSystemPrompt = system(result.systemPrompt);
      }
      await runtime.flush(child);
    }
  } finally { budget.finish(); }
  prompt.applyTools();
  return { systemPrompt: system(prompt.render()), messages };
}

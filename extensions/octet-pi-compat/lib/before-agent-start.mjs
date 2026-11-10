import { bounded, facade, fields, invalid } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';
import { customMessage } from './custom-messages.mjs';
import { HookBudget } from './handler-budget.mjs';
import { visibleImages } from './image-content.mjs';
import { promptOptions } from './system-prompt-options.mjs';

function system(text) {
  bounded(text, 'before_agent_start systemPrompt', 262144, { controls: true });
  if (text.includes('\0')) invalid('before_agent_start systemPrompt contains NUL');
  return text;
}

// Pi applies every before_agent_start result to the run it is composing: the
// system prompt it names replaces the prompt, the message it carries is
// appended. A handler that only settles after this hook returned can no longer
// change that prompt, so name what was lost instead of dropping it in silence.
function lateResult(result) {
  const parts = [];
  if (result?.systemPrompt !== undefined) parts.push('system prompt');
  if (result?.message !== undefined) parts.push('message');
  return parts.length ? parts.join(' and ') : undefined;
}

// Native BeforePrompt owns applying this per-run replacement. It is never an
// additive context fragment, persisted Session edit or provider payload override.
export async function beforeAgentStart(runtime, payload, store) {
  runtime.require('before_prompt_state_v1'); runtime.assertSessionOwner(store);
  fields(payload, ['prompt', 'system_prompt', 'images'], 'before_agent_start native payload');
  bounded(payload.prompt, 'before_agent_start prompt', 262144, { controls: true });
  // Pi's BeforeAgentStartUserEvent names the images attached to this run and
  // omits the member when the run attached none. The native payload carries the
  // same canonical media the `input` hook uses, so it converts the same way.
  const images = visibleImages(payload.images, 'before_agent_start native images');
  const handlers = [...runtime.events.get('before_agent_start') || []];
  const prompt = promptOptions(runtime, store, system(payload.system_prompt), handlers[0]?.factory);
  const messages = [];
  store.providerContext = true;
  // Pi has no handler timeout for this event: each handler runs until it
  // settles, its result is applied to the prompt being composed, and a throwing
  // one is reported and skipped while the turn carries on. The runner is here for
  // the hook's own boundaries — a cancelled run stops the wait at once, a quit
  // preempts it — and to name a transformation that arrives after the turn moved
  // on instead of dropping it.
  const hooks = new HookBudget(runtime, store, 'before_agent_start');
  try {
    for (const entry of handlers) {
      store.controller.signal.throwIfAborted(); runtime.assertSessionOwner(store);
      const child = { ...store, factory: entry.factory };
      const event = facade({ type: 'before_agent_start', messageType: 'user', prompt: payload.prompt,
        get systemPrompt() { return system(prompt.render()); }, systemPromptOptions: prompt.options,
        ...(images === undefined ? {} : { images }) }, 'before_agent_start event');
      const context = createContext(runtime, child);
      Object.defineProperty(context, 'getSystemPrompt', { value: () => { runtime.assertSessionOwner(child); child.controller.signal.throwIfAborted(); return system(prompt.render()); } });
      let result;
      try {
        const run = await cancellable(hooks.run(entry, child, () => runtime.scope.run(child, () => entry.handler(event, context)), { onLateResult: (_entry, value) => lateResult(value) }), store.controller.signal);
        // No budget means no drop: a settled handler always has an answer to
        // apply, and only a quit preempting the hook leaves one unanswered.
        if (run.ran) result = run.value;
      } catch (error) {
        if (Number.isInteger(error?.code) || store.controller.signal.aborted) throw error;
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
  } finally { hooks.finish(); }
  prompt.applyTools();
  return { systemPrompt: system(prompt.render()), messages };
}

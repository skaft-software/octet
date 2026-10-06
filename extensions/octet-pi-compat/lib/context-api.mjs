// Pi 1.0.2 context facts and raw-input transforms. Rust supplies every fact;
// extension request cancellation is not a substitute for aborting the agent.
import { bounded, facade, fields, invalid, plainJSON, strict, unsupported } from './errors.mjs';
import { createContext } from './api.mjs';
import { canonicalToPi, cancellable, piToCanonical } from './provider-context.mjs';

function fact(runtime, store, key, api) {
  runtime.assertSessionOwner(store);
  if (!Object.hasOwn(store.state.host, key)) unsupported(api, `authoritative ${key} snapshot required`);
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
      return trusted;
    },
    getSystemPromptOptions() {
      if (store.method !== 'command/execute') unsupported('ctx.getSystemPromptOptions', 'command context only');
      return plainJSON(fact(runtime, store, 'system_prompt_options', 'ctx.getSystemPromptOptions'), 'system prompt options', 524288);
    },
  };
}
export function getSettings(runtime, store) {
  return plainJSON(fact(runtime, store, 'settings', 'pi.getSettings'), 'settings', 524288);
}

function inputImages(images) {
  if (images === undefined) return undefined;
  if (!Array.isArray(images) || images.length > 256) invalid('input images');
  const parts = piToCanonical([{ role: 'user', content: images }])[0].User.content;
  if (parts.some(part => !part.Media)) invalid('input images must contain images only');
  return parts.map(part => part.Media);
}
function visibleImages(media) {
  if (media === undefined || media === null) return undefined;
  if (!Array.isArray(media) || media.length > 256) invalid('native input images');
  return canonicalToPi([{ User: { content: media.map(Media => ({ Media })) } }])[0].content;
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
  for (const entry of handlers) {
    signal.throwIfAborted(); runtime.assertSessionOwner(store);
    const child = { ...store, factory: entry.factory };
    let result;
    try {
      result = await cancellable(runtime.scope.run(child, () => entry.handler(facade({
        type: 'input', text, images, source: payload.source, streamingBehavior: payload.streaming_behavior ?? undefined,
      }, 'input event'), createContext(runtime, child))), signal);
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
  await cancellable(runtime.flush(store), signal);
  signal.throwIfAborted(); runtime.assertSessionOwner(store);
  return text !== originalText || JSON.stringify(images) !== originalImages
    ? { action: 'transform', text, ...(images === undefined ? {} : { images: inputImages(images) }) }
    : { action: 'continue' };
}

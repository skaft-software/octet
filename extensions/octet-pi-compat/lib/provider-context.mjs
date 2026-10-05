// Pi context ordering/custom-message conversion follows pinned Pi 1.0
// cd32f7725fdbddbaecdff5b1e68491563394e0ca (runner.ts/messages.ts), MIT LICENSE.pi.
// Canonical request projection only: never mutate tools, routing or the Session.
import { bounded, fields, invalid, ownerKey, plainJSON, strict, unsupported } from './errors.mjs';
import { createContext } from './api.mjs';
import { prepareToolLoadout } from './tools.mjs';

const apis = { open_ai_responses: 'openai-responses', open_ai_chat: 'openai-completions', anthropic_messages: 'anthropic-messages', bedrock_converse: 'bedrock-converse-stream', google_generative_ai: 'google-generative-ai', mistral_conversations: 'mistral-conversations', pi_messages: 'pi-messages' };
const protocols = Object.fromEntries(Object.entries(apis).map(([key, value]) => [value, key]));
const text = value => bounded(value, 'context text', 786432, { controls: true });
function array(value, label) {
  if (!Array.isArray(value) || value.length > 8192) invalid(`${label} must be a bounded array`);
  for (let i = 0; i < value.length; i++) if (!Object.hasOwn(value, i)) invalid(`${label} must not be sparse`);
  for (const key of Reflect.ownKeys(value)) if (key !== 'length' && !(typeof key === 'string' && /^(0|[1-9][0-9]*)$/.test(key) && Number(key) < value.length)) invalid(`${label} has extra properties`);
  return value;
}
function record(value, keys, label) {
  fields(value, keys, label);
  if (![Object.prototype, null].includes(Object.getPrototypeOf(value))) invalid(`${label} must be plain`);
  for (const key of Reflect.ownKeys(value)) if (typeof key !== 'string' || !keys.includes(key) || !Object.getOwnPropertyDescriptor(value, key).enumerable) unsupported(`${label} field ${String(key)}`);
  return value;
}
function variant(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).length !== 1) invalid(label);
  return Object.entries(value)[0];
}
const name = value => bounded(value, 'context name', 256);

// Pi 1.0.2 ImageContent can represent inline images only. URL/provider references,
// detail hints and audio need a native replay binding, not an invented Pi field.
function imageData(value) {
  const data = bounded(value, 'context image data', 786432);
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(data)
      || Buffer.from(data, 'base64').toString('base64') !== data) invalid('context image base64');
  return data;
}
function imageType(value) {
  const mime = bounded(value, 'context image MIME type', 256);
  if (!/^image\/[a-zA-Z0-9!#$&^_.+\-]+$/.test(mime)) invalid('context image MIME type');
  return mime;
}
function mediaToPi(media) {
  const [kind, value] = variant(media, 'canonical media');
  if (kind !== 'Image') unsupported(`context media ${kind}`, 'Pi 1.0.2 has no audio content type');
  record(value, ['source', 'media_type', 'detail'], 'canonical image');
  if (value.detail != null) unsupported('context image detail', 'Pi ImageContent has no detail hint');
  const [source, data] = variant(value.source, 'canonical image source');
  if (source !== 'Inline') unsupported(`context image source ${source}`, 'Pi ImageContent requires inline bytes');
  if (value.media_type == null) unsupported('context image without MIME type', 'cannot invent image format');
  return { type: 'image', data: imageData(data), mimeType: imageType(value.media_type) };
}
function contentToPi(part, label) {
  const [kind, value] = variant(part, label);
  if (kind === 'Text') return { type: 'text', text: text(value) };
  if (kind === 'Media') return mediaToPi(value);
  unsupported(`${label} ${kind}`, 'no lossless Pi content binding');
}

// Ephemeral structural provenance, never visible Pi fields or Session IDs.
// Keep a native mixed user/tool batch intact when retained messages are sent
// back after an unrelated context edit. Copies/new messages use Pi's ordinary
// one-message conversion; they cannot claim the original native boundary.
const projectionOrigins = new WeakMap();

export function canonicalToPi(messages, calls = new Map(), { observation = false } = {}) {
  const output = [];
  for (const message of array(messages, 'canonical messages')) {
    const start = output.length;
    const [role, body] = variant(message, 'canonical message');
    if (role === 'Assistant') {
      record(body, ['content', 'model', 'protocol'], 'canonical assistant');
      if (!apis[body.protocol]) unsupported(`context protocol ${body.protocol}`);
      const content = array(body.content, 'assistant content').map(part => {
        const [kind, value] = variant(part, 'assistant part');
        if (kind === 'Text') return { type: 'text', text: text(value) };
        if (kind === 'ToolCall') {
          record(value, ['id', 'name', 'arguments_json', 'async', 'argument_error'], 'canonical tool call');
          // Read-only lifecycle observations do not control native scheduling.
          // Context rewrites still require its lossless replay provenance.
          if ((!observation && value.async) || value.argument_error != null) unsupported('context tool call scheduling/argument metadata');
          const id = name(value.id), tool = name(value.name), args = JSON.parse(value.arguments_json);
          if (!args || typeof args !== 'object' || Array.isArray(args)) invalid('tool arguments object');
          calls.set(id, tool); return { type: 'toolCall', id, name: tool, arguments: args };
        }
        if (kind === 'Reasoning') {
          record(value, ['text', 'state'], 'canonical reasoning');
          if (value.text === null) unsupported('context absent reasoning text', 'cannot replace absent text with an invented empty string');
          const thinking = { type: 'thinking', thinking: text(value.text) };
          if (value.state != null) {
            const state = record(value.state, ['protocol', 'model', 'kind'], 'context opaque reasoning');
            if (!['anthropic_messages', 'bedrock_converse'].includes(body.protocol)
                || state.protocol !== body.protocol || state.model !== body.model) unsupported('context opaque reasoning continuation', 'signature producer must match the assistant');
            const [kind, signature] = variant(state.kind, 'reasoning state kind');
            if (kind !== 'AnthropicSignature') unsupported('context opaque reasoning continuation', 'no lossless Pi signature binding for this state');
            record(signature, ['signature'], 'reasoning signature');
            thinking.thinkingSignature = text(signature.signature);
          }
          return thinking;
        }
        unsupported(`context assistant part ${kind}`, 'no lossless Pi content binding');
      });
      // Canonical messages carry neither timestamps nor provider usage. Do not
      // fabricate those facts or assign the current route to historical output.
      output.push({ role: 'assistant', model: name(body.model), api: apis[body.protocol], content });
    } else if (role === 'User') {
      record(body, ['content'], 'canonical user');
      let content = [];
      const flush = () => { if (content.length) { output.push({ role: 'user', content }); content = []; } };
      for (const part of array(body.content, 'user content')) {
        const [kind, value] = variant(part, 'user part');
        if (kind === 'Text' || kind === 'Media') content.push(contentToPi(part, 'context user part'));
        else if (kind === 'ToolResult') {
          flush(); record(value, ['tool_call_id', 'content', 'is_error', 'added_tool_names'], 'canonical tool result');
          // Read-only observations report result content, not native registry changes.
          // Context rewrites still cannot replay or discard that host-owned metadata.
          if (!observation && value.added_tool_names != null) unsupported('context tool registry metadata');
          const id = name(value.tool_call_id), toolName = calls.get(id);
          if (!toolName) invalid('canonical tool result has no preceding call');
          if (typeof value.is_error !== 'boolean') invalid('tool result error flag');
          const parts = array(value.content, 'tool result content').map(part => contentToPi(part, 'context tool result part'));
          output.push({ role: 'toolResult', toolCallId: id, toolName, content: parts, isError: value.is_error });
        } else unsupported(`context user part ${kind}`, 'no lossless Pi content binding');
      }
      flush();
      if (!body.content.length) output.push({ role: 'user', content: [] });
    } else unsupported(`canonical context role ${role}`);
    const projected = output.slice(start);
    const origin = { canonical: plainJSON(message, 'canonical source message', 786432), projected: JSON.stringify(projected), count: projected.length };
    projected.forEach((item, index) => projectionOrigins.set(item, { origin, index }));
  }
  return output;
}

function userParts(content) {
  if (typeof content === 'string') return [{ Text: text(content) }];
  return array(content, 'Pi user content').map(part => {
    if (part?.type === 'image') {
      record(part, ['type', 'data', 'mimeType'], 'Pi image part');
      return { Media: { Image: { source: { Inline: imageData(part.data) }, media_type: imageType(part.mimeType), detail: null } } };
    }
    record(part, ['type', 'text'], 'Pi text part');
    if (part.type !== 'text') unsupported(`context Pi content ${part.type}`);
    return { Text: text(part.text) };
  });
}
export function piToCanonical(messages) {
  const output = [];
  for (const message of array(messages, 'Pi messages')) {
    if (message?.role === 'user' || message?.role === 'custom') {
      record(message, message.role === 'custom' ? ['role', 'content', 'customType', 'display', 'details', 'timestamp'] : ['role', 'content', 'timestamp'], 'Pi user/custom message');
      // Pi's real convertToLlm intentionally projects custom messages as users;
      // display/details/customType are transcript metadata, not provider roles.
      output.push({ User: { content: userParts(message.content) } });
    } else if (message?.role === 'assistant') {
      record(message, ['role', 'content', 'model', 'api', 'provider', 'timestamp', 'usage', 'stopReason', 'errorMessage'], 'Pi assistant message');
      const protocol = protocols[message.api]; if (!protocol) unsupported(`Pi context API ${message.api}`);
      const content = array(message.content, 'Pi assistant content').map(part => {
        if (part?.type === 'text') { record(part, ['type', 'text'], 'Pi text part'); return { Text: text(part.text) }; }
        if (part?.type === 'thinking') {
          record(part, ['type', 'thinking', 'thinkingSignature'], 'Pi thinking part');
          let state = null;
          if (Object.hasOwn(part, 'thinkingSignature')) {
            if (!['anthropic_messages', 'bedrock_converse'].includes(protocol)) unsupported('Pi opaque reasoning signature', 'no lossless native signature binding for this API');
            state = { protocol, model: name(message.model), kind: { AnthropicSignature: { signature: text(part.thinkingSignature) } } };
          }
          return { Reasoning: { text: text(part.thinking), state } };
        }
        if (part?.type === 'toolCall') {
          record(part, ['type', 'id', 'name', 'arguments'], 'Pi tool call');
          const args = plainJSON(part.arguments, 'tool arguments', 262144);
          if (!args || typeof args !== 'object' || Array.isArray(args)) invalid('tool arguments object');
          return { ToolCall: { id: name(part.id), name: name(part.name), arguments_json: JSON.stringify(args) } };
        }
        unsupported(`Pi assistant content ${part?.type}`);
      });
      output.push({ Assistant: { content, model: name(message.model), protocol } });
    } else if (message?.role === 'toolResult') {
      record(message, ['role', 'toolCallId', 'toolName', 'content', 'details', 'isError', 'timestamp'], 'Pi tool result');
      if (typeof message.isError !== 'boolean') invalid('Pi tool result error flag');
      output.push({ User: { content: [{ ToolResult: { tool_call_id: name(message.toolCallId), content: userParts(message.content), is_error: message.isError } }] } });
    } else unsupported(`Pi context role ${message?.role}`, 'canonical provider role has no faithful binding');
  }
  // All parts must pass ordinary validation before reusing source structure:
  // JSON equality alone would hide symbol/nonenumerable/undefined mutations.
  const restored = [];
  for (let index = 0; index < messages.length; index++) {
    const source = projectionOrigins.get(messages[index]);
    if (source?.index === 0) {
      const group = messages.slice(index, index + source.origin.count);
      if (group.length === source.origin.count && group.every((item, offset) => {
        const next = projectionOrigins.get(item);
        return next?.origin === source.origin && next.index === offset;
      }) && JSON.stringify(group) === source.origin.projected) {
        restored.push(source.origin.canonical); index += group.length - 1; continue;
      }
    }
    restored.push(output[index]);
  }
  return plainJSON(restored, 'canonical context projection', 786432);
}

export async function cancellable(work, signal) {
  signal.throwIfAborted(); let abort;
  try { return await Promise.race([work, new Promise((_, reject) => {
    abort = () => reject(signal.reason); signal.addEventListener('abort', abort, { once: true });
  })]); } finally { signal.removeEventListener('abort', abort); }
}
export async function projectContext(runtime, params, store) {
  runtime.require('session_entries');
  if (!runtime.metadata().hooks.includes('provider_context')) invalid('unknown hook provider_context');
  ownerKey(params.context?.resource_owner);
  record(params.payload, ['request', 'preparation'], 'provider context payload');
  const { request, preparation } = params.payload;
  record(preparation, ['resource_owner', 'session_id', 'head', 'tool_generation'], 'context preparation');
  if (preparation.resource_owner !== params.context.resource_owner.session_id || typeof preparation.session_id !== 'string' || !Number.isSafeInteger(preparation.tool_generation) || preparation.tool_generation < 0) invalid('context preparation identity');
  runtime.bind(params, store);
  if (!store.leaf?.grant || store.leaf.grant.expected_head !== preparation.head) unsupported('provider_context', 'matching native session_leaf preparation required');
  store.providerContext = true;
  const signal = store.controller.signal;
  const live = () => { signal.throwIfAborted(); runtime.assertOwner(store); };
  return cancellable(runtime.queued(store, async () => {
    live();
    const tools = prepareToolLoadout(runtime, store, request.tools);
    if (!runtime.events.get('context')?.length && !runtime.events.get('context_with_system')?.length) return { disposition: { action: 'continue' }, context: [], notifications: [], ...(tools ? { provider_context: { messages: request.messages, system: request.system ?? null, tools } } : {}) };
    let system = request.system === null || request.system === undefined ? null : text(request.system);
    const originalSystem = system;
    store.state.host.system_prompt = system ?? '';
    store.state.host.session_id = preparation.session_id;
    let messages = canonicalToPi(request.messages);
    const original = JSON.stringify(messages);
    // Snapshot handlers just once, retaining registration/factory order. Pi's
    // context event hides system messages, retaining the real native system.
    for (const entry of [...runtime.events.get('context') || []]) {
      live(); const child = { ...store, factory: entry.factory }, visible = messages.slice();
      let result;
      try { result = await cancellable(runtime.scope.run(child, () => entry.handler(
        strict({ type: 'context', messages: visible }, 'context event'), createContext(runtime, child))), signal); }
      catch (error) {
        live(); if (Number.isInteger(error?.code)) throw error;
        runtime.backgroundError(new Error(`context factory ${runtime.config.extensions[entry.factory]}: ${String(error?.message || error)}`));
        continue;
      }
      live();
      if (result !== undefined) { record(result, ['messages'], 'context result'); if (result.messages !== undefined) messages = array(result.messages, 'context result messages'); else messages = visible; }
      else messages = visible;
      // Refuse unsupported mutations before exposing them to another callback.
      piToCanonical(messages);
    }
    if (runtime.events.has('context_with_system')) {
      const tools = array(request.tools || [], 'context tools').map(tool => {
        record(tool, ['name', 'description', 'parameters', 'async', 'constrained_sampling'], 'canonical context tool');
        if (tool.async) unsupported('context async tool declarations', 'Pi Tool has no scheduling field');
        return { name: name(tool.name), description: text(tool.description), parameters: plainJSON(tool.parameters, 'context tool schema', 262144),
          ...(tool.constrained_sampling == null ? {} : { constrainedSampling: plainJSON(tool.constrained_sampling, 'constrained sampling') }) };
      });
      const originalTools = JSON.stringify(tools);
      let full = [strict({ role: 'system', content: system ?? '', toolsAdded: tools, sections: undefined, toolsRemoved: undefined }, 'system message'), ...messages];
      for (const entry of [...runtime.events.get('context_with_system') || []]) {
        live(); const child = { ...store, factory: entry.factory };
        let result;
        try { result = await cancellable(runtime.scope.run(child, () => entry.handler(
          strict({ type: 'context_with_system', messages: full }, 'context_with_system event'), createContext(runtime, child))), signal); }
        catch (error) {
          live(); if (Number.isInteger(error?.code)) throw error;
          runtime.backgroundError(new Error('context_with_system callback failed (private context details redacted)'));
          continue;
        }
        live();
        if (result !== undefined) { record(result, ['messages'], 'context_with_system result'); if (result.messages != null) full = array(result.messages, 'context_with_system messages'); }
        const leading = full[0]?.role === 'system' ? full[0] : undefined;
        if (!leading && originalTools !== '[]') unsupported('context_with_system removed tool declarations', 'native preparation cannot replace the advertised tool snapshot');
        if (leading) {
          record(leading, ['role', 'content', 'toolsAdded', 'sections', 'toolsRemoved', 'timestamp'], 'system message');
          if (JSON.stringify(leading.toolsAdded ?? []) !== originalTools || leading.toolsRemoved?.length) unsupported('context_with_system tool changes', 'native preparation cannot replace the advertised tool snapshot');
          const content = typeof leading.content === 'string' ? text(leading.content) : array(leading.content, 'system content').map(part => {
            record(part, ['type', 'text'], 'system text part'); if (part.type !== 'text') invalid('system content must be text'); return text(part.text);
          }).join('\n');
          const sections = leading.sections ?? {}; if (!sections || typeof sections !== 'object' || Array.isArray(sections)) invalid('system sections');
          system = [content, ...Object.values(sections).filter(value => value !== null).map(text)].filter(value => value.length).join('\n\n');
        } else {
          system = null;
          runtime.backgroundError(new Error('context_with_system handler removed the leading system message'));
        }
        messages = leading ? full.slice(1) : full;
        if (messages.some(message => message.role === 'system')) unsupported('context_with_system later system messages', 'native canonical messages cannot represent mid-transcript system changes');
        piToCanonical(messages);
      }
    }
    await cancellable(runtime.flush(store), signal); live();
    return { disposition: { action: 'continue' }, context: [], notifications: [],
      ...(JSON.stringify(messages) === original && system === originalSystem && tools === undefined ? {} : { provider_context: { messages: piToCanonical(messages), system, ...(tools === undefined ? {} : { tools }) } }) };
  }), signal);
}

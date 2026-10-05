// Pi message ingestion. Preserve string-vs-block content in the durable entry.
import { bounded, fields, invalid, plainJSON, unsupported } from './errors.mjs';

export function customMessage(message) {
  fields(message, ['customType', 'content', 'display', 'details'], 'message');
  return {
    custom_type: bounded(message.customType, 'customType', 128),
    content: messageContent(message.content ?? []),
    display: Boolean(message.display),
    ...(message.details === undefined ? {} : { details: plainJSON(message.details, 'message details') }),
  };
}

export function customMessageParams(message, options = {}) {
  fields(options, ['triggerTurn', 'deliverAs'], 'sendMessage options');
  if (options.triggerTurn !== undefined && typeof options.triggerTurn !== 'boolean') invalid('sendMessage triggerTurn');
  return { ...customMessage(message), ...delivery(options.deliverAs, ['steer', 'followUp', 'nextTurn']),
    ...(options.triggerTurn === undefined ? {} : { trigger_turn: options.triggerTurn }) };
}

export function userMessageParams(content, options = {}) {
  fields(options, ['deliverAs'], 'sendUserMessage options');
  const value = messageContent(content);
  const text = typeof value === 'string' ? value : value.map(part => part.text).join('\n');
  return { text: bounded(text, 'user message', 262144), ...delivery(options.deliverAs, ['steer', 'followUp']) };
}

function messageContent(content) {
  if (typeof content === 'string') return bounded(content, 'message', 262144);
  if (!Array.isArray(content) || content.length > 256) invalid('message content must be a string or array');
  let bytes = 0;
  return content.map(part => {
    fields(part, ['type', 'text'], 'message content part');
    if (part.type !== 'text') unsupported(`message content ${part.type}`, 'image message content is not yet implemented');
    const text = bounded(part.text, 'message content text', 262144);
    bytes += Buffer.byteLength(text);
    if (bytes > 262144) invalid('message content exceeds bounds');
    return { type: 'text', text };
  });
}

function delivery(deliverAs, allowed) {
  if (deliverAs === undefined) return {};
  if (!allowed.includes(deliverAs)) invalid(`deliverAs ${deliverAs}`);
  return { deliver_as: { steer: 'steer', followUp: 'follow_up', nextTurn: 'next_turn' }[deliverAs] };
}

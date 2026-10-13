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
  return { ...(typeof value === 'string' ? { text: value } : { text: '', content: value }),
    ...delivery(options.deliverAs, ['steer', 'followUp']) };
}

function messageContent(content) {
  if (typeof content === 'string') return bounded(content, 'message', 262144);
  if (!Array.isArray(content) || content.length > 256) invalid('message content must be a string or array');
  let bytes = 0, images = 0, imageBytes = 0;
  return content.map(part => {
    if (part?.type === 'image') {
      fields(part, ['type', 'data', 'mimeType'], 'message image part');
      // The existing inline protocol frame remains the tighter transport bound.
      const data = bounded(part.data, 'message image data', 786432);
      if (!data || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(data)
          || Buffer.from(data, 'base64').toString('base64') !== data) invalid('message image must be canonical base64');
      const mimeType = bounded(part.mimeType, 'message image MIME type', 256);
      if (!['image/png', 'image/jpeg', 'image/gif', 'image/webp'].includes(mimeType)) unsupported(`message image ${mimeType}`);
      imageBytes += Buffer.from(data, 'base64').length;
      if (++images > 8 || imageBytes > 20 * 1024 * 1024) invalid('message image batch exceeds bounds');
      return { type: 'image', data, mimeType };
    }
    fields(part, ['type', 'text'], 'message content part');
    if (part.type !== 'text') unsupported(`message content ${part.type}`);
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

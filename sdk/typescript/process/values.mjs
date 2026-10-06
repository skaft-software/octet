import {json, object, own} from './schema.mjs';

export function closed(value, required, optional = []) {
  if (!object(value) || required.some(k => !own(value, k)) || Object.keys(value).some(k => ![...required, ...optional].includes(k))) throw new TypeError('Invalid closed record');
}
export function text(value, limit = 4096, empty = false, controls = false) {
  if (typeof value !== 'string' || (!empty && !value) || Buffer.byteLength(value) > limit || /\p{Surrogate}/u.test(value) ||
      (controls ? /[\x00-\x1f\x7f-\x9f]/ : /[\x00-\x08\x0b-\x1f\x7f-\x9f]/).test(value)) throw new TypeError('Invalid bounded text');
}
export const opaque = value => typeof value === 'string' && /^[!-~]{1,128}$/.test(value);
export const nominal = value => typeof value === 'string' && /^[A-Za-z][A-Za-z0-9_.-]{0,127}$/.test(value);
export const uint = value => Number.isSafeInteger(value) && value >= 0;
function list(value, max) {
  if (!Array.isArray(value) || value.length > max) throw new TypeError('Invalid bounded list');
  return value;
}
function location(value) {
  closed(value, ['source', 'span']);
  const s = value.source;
  if (s?.kind === 'workspace') {
    closed(s, ['kind', 'path', 'revision']); text(s.path, 4096, false, true);
    if (/[\\:]/.test(s.path) || s.path.split('/').some(p => ['', '.', '..'].includes(p)) || !/^[0-9a-f]{64}$/.test(s.revision)) throw new TypeError('Invalid revision-bound workspace source');
  } else {
    closed(s, ['kind', 'id']);
    if (!['blob', 'artifact'].includes(s.kind) || !opaque(s.id)) throw new TypeError('Invalid source');
  }
  closed(value.span, ['start_byte', 'end_byte']);
  if (!uint(value.span.start_byte) || !uint(value.span.end_byte) || value.span.start_byte > value.span.end_byte) throw new TypeError('Invalid byte span');
}
export function validateDiagnostics(values) {
  list(values, 32); json(values, 65536);
  for (const d of values) {
    closed(d, ['severity', 'code', 'message'], ['primary', 'related', 'fixes', 'attachments']);
    if (!['error', 'warning', 'info', 'hint'].includes(d.severity) || !nominal(d.code)) throw new TypeError('Invalid diagnostic');
    text(d.message);
    if (own(d, 'primary')) location(d.primary);
    for (const r of list(own(d, 'related') ? d.related : [], 16)) { closed(r, ['message', 'location']); text(r.message); location(r.location); }
    for (const f of list(own(d, 'fixes') ? d.fixes : [], 8)) {
      closed(f, ['title', 'edits']); text(f.title); list(f.edits, 16);
      if (!f.edits.length) throw new TypeError('Fix requires edits');
      for (const e of f.edits) {
        closed(e, ['location', 'replacement']); location(e.location); text(e.replacement, 16384, true);
        if (e.location.source.kind !== 'workspace') throw new TypeError('Fix requires a workspace revision');
      }
    }
    for (const a of list(own(d, 'attachments') ? d.attachments : [], 16)) {
      closed(a, ['kind', 'id'], ['label']);
      if (!['blob', 'artifact'].includes(a.kind) || !opaque(a.id)) throw new TypeError('Invalid attachment');
      if (own(a, 'label')) text(a.label);
    }
  }
}
export function diagnosticSummary(values) {
  validateDiagnostics(values);
  const all = values.slice(0, 8).map(d => `${d.severity}[${d.code}]: ${d.message.replace(/[\n\t]/g, ' ')}`).join('\n');
  let result = '', bytes = 0;
  for (const ch of all) { bytes += Buffer.byteLength(ch); if (bytes > 4096) break; result += ch; }
  return result;
}
export function mediaParts(parts) {
  list(parts, 254); json(parts);
  for (const part of parts) {
    const label = part?.type === 'image' ? 'alt' : 'transcript';
    closed(part, ['type', 'artifact_id', 'mime_type'], [label]);
    if (!['image', 'audio'].includes(part.type) || !opaque(part.artifact_id) || typeof part.mime_type !== 'string' || !part.mime_type.startsWith(`${part.type}/`)) throw new TypeError('Invalid media part');
    text(part.mime_type, 128, false, true);
    if (own(part, label)) text(part[label], 65536, true);
  }
}
export const blobSchema = Object.freeze({type: 'object', properties: {
  $blob: {type: 'string', minLength: 1, maxLength: 128}, bytes: {type: 'integer', minimum: 0, maximum: Number.MAX_SAFE_INTEGER},
  digest: {type: 'object', properties: {algorithm: {type: 'string', enum: ['sha256']}, value: {type: 'string', minLength: 64, maxLength: 64}}, required: ['algorithm', 'value'], additionalProperties: false},
  media_type: {type: 'string', minLength: 1, maxLength: 128},
}, required: ['$blob', 'bytes', 'digest', 'media_type'], additionalProperties: false});
export function validateBlob(value) {
  closed(value, ['$blob', 'bytes', 'digest', 'media_type']); closed(value.digest, ['algorithm', 'value']);
  if (!opaque(value.$blob) || !uint(value.bytes) || value.digest.algorithm !== 'sha256' || !/^[0-9a-f]{64}$/.test(value.digest.value) ||
      typeof value.media_type !== 'string' || !/^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+$/.test(value.media_type) || value.media_type.length > 128) throw new TypeError('Invalid BlobRef');
  return value;
}

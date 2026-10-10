import { bounded, invalid, ownerKey, rpcError } from './errors.mjs';

const controls = /[\x00-\x1f\x7f-\x9f]/u;
export function leafGrant(value, owner, previous, head) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) invalid('session_leaf grant');
  if (!/^[a-f0-9]{64}$/.test(value.grant_id) || !Number.isSafeInteger(value.activation_epoch) || value.activation_epoch < 0) invalid('session_leaf grant identity');
  bounded(value.operation_id, 'session_leaf operation', 256);
  if (!value.operation_id || controls.test(value.operation_id)) invalid('session_leaf operation');
  if (ownerKey(value.owner) !== ownerKey(owner)) throw rpcError(-32002, 'not_foreground_owner session_leaf grant');
  for (const field of ['session_id', 'extension_instance_id']) {
    bounded(value.owner[field], `session_leaf owner ${field}`, 256);
    if (controls.test(value.owner[field])) invalid('session_leaf owner');
  }
  if (value.expected_head !== null) {
    bounded(value.expected_head, 'session_leaf expected head', 256);
    if (!value.expected_head || controls.test(value.expected_head)) invalid('session_leaf expected head');
  }
  if (previous && (value.activation_epoch !== previous.activation_epoch || value.operation_id !== previous.operation_id || value.grant_id === previous.grant_id || value.expected_head !== head)) invalid('session_leaf successor fence');
  // Preserve host-issued fields; peer fields do not mint host authority.
  return Object.freeze({ ...value, owner: Object.freeze({ ...value.owner }) });
}
export function entryPayload(type, data) {
  bounded(type, 'entry type', 128);
  if (!type.trim() || controls.test(type)) invalid('entry type must be nonempty without controls');
  // Match Pi's JSON persistence: optional object members are omitted. Functions,
  // exotic prototypes and undefined array members are not durable JSON values.
  let nodes = 0;
  const visit = (value, depth) => {
    if (depth > 16 || ++nodes > 256) invalid('entry data exceeds depth16/nodes256');
    if (value === null || typeof value === 'boolean') return value;
    if (typeof value === 'string') {
      bounded(value, 'entry string value', 16384, { controls: true });
      if (/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/u.test(value)) invalid('entry string value contains controls');
      return value;
    }
    if (typeof value === 'number' && Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value))) return value;
    if (!value || typeof value !== 'object' || !Array.isArray(value) && ![Object.prototype, null].includes(Object.getPrototypeOf(value))) invalid('entry data must be plain JSON');
    if (Array.isArray(value)) return Array.from(value, child => visit(child, depth + 1));
    const result = Object.create(null);
    for (const [key, child] of Object.entries(value)) {
      bounded(key, 'entry key', 256);
      if (controls.test(key)) invalid('entry key contains controls');
      if (child !== undefined) result[key] = visit(child, depth + 1);
    }
    return result;
  };
  const envelope = visit({ entry_type: type, data }, 0);
  if (!Object.hasOwn(envelope, 'data')) invalid('entry data is required');
  if (Buffer.byteLength(JSON.stringify(envelope)) > 16384) throw rpcError(-32602, 'bounds_exceeded entry envelope16KiB');
  return envelope.data;
}
export function appendReply(result, grant, owner, routed = false) {
  if (!result || typeof result !== 'object' || Array.isArray(result)) invalid('session append reply');
  bounded(result.entry_id, 'committed entry id', 256);
  if (!result.entry_id || controls.test(result.entry_id)) invalid('committed entry id');
  if (!grant) {
    if (Object.keys(result).some(key => key !== 'entry_id')) invalid('unbound session append reply');
    return { entryId: result.entry_id };
  }
  if (Object.keys(result).some(key => !['entry_id', 'head', 'successor', ...(routed ? ['entry', 'previous_revision', 'view_revision', 'previous_head'] : [])].includes(key))) invalid('session append reply fields');
  bounded(result.head, 'committed head', 256);
  if (result.head !== result.entry_id) invalid('session append committed head');
  const successor = result.successor === null ? null : leafGrant(result.successor, owner, grant, result.head);
  return { entryId: result.entry_id, head: result.head, successor };
}

export const rpcError = (code, message) => Object.assign(new Error(message), { code });
export function unsupported(name, reason = 'no corresponding negotiated octet contract') {
  throw rpcError(-32601, `unsupported_feature ${name}: ${reason}`);
}
export function invalid(message) { throw rpcError(-32602, `invalid_request ${message}`); }
export function bounded(text, name, max, { controls = false } = {}) {
  if (typeof text !== 'string' || /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(text)) invalid(`${name} must be UTF-8 text`);
  if (Buffer.byteLength(text) > max) throw rpcError(-32602, `bounds_exceeded ${name}`);
  if (!controls && /[\x00-\x08\x0b-\x1f\x7f-\x9f]/u.test(text)) invalid(`${name} contains terminal controls`);
  return text;
}
export function fields(value, allowed, name) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) invalid(`${name} must be an object`);
  for (const key of Object.keys(value)) if (!allowed.includes(key)) unsupported(`${name}.${key}`, 'option would not be honored');
}
export function strict(object, label) {
  return new Proxy(object, { get(target, key, receiver) {
    if (Reflect.has(target, key)) return Reflect.get(target, key, receiver);
    if (typeof key === 'symbol' || key === 'then' || key === 'toJSON') return undefined;
    return unsupported(`${label}.${key}`);
  } });
}
export function plainJSON(value, name, max = 65536) {
  const visit = (v, depth) => {
    if (depth > 32) invalid(`${name} exceeds JSON depth`);
    if (v === null || typeof v === 'boolean' || typeof v === 'string') return;
    if (typeof v === 'number' && Number.isFinite(v) && (!Number.isInteger(v) || Number.isSafeInteger(v))) return;
    if (typeof v !== 'object' || (!Array.isArray(v) && ![Object.prototype, null].includes(Object.getPrototypeOf(v)))) invalid(`${name} must be plain JSON`);
    for (const x of Object.values(v)) visit(x, depth + 1);
  };
  visit(value, 0);
  const encoded = JSON.stringify(value);
  if (Buffer.byteLength(encoded) > max) throw rpcError(-32602, `bounds_exceeded ${name}`);
  return JSON.parse(encoded);
}
export function ownerKey(owner) {
  if (!owner || typeof owner.session_id !== 'string' || !owner.session_id || typeof owner.extension_instance_id !== 'string' || !owner.extension_instance_id || !Number.isSafeInteger(owner.process_generation) || owner.process_generation < 0) invalid('complete resource_owner required');
  return JSON.stringify([owner.session_id, owner.extension_instance_id, owner.process_generation]);
}

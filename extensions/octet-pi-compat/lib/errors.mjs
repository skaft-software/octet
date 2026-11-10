export const rpcError = (code, message) => Object.assign(new Error(message), { code });
// An API name this bridge (or the host) owns: a host method, a negotiated
// feature, or a Pi context member an extension called. It is fixed vocabulary
// from adapter/native source, so a user-visible notice may name it.
const API_NAME = /^[A-Za-z_$][A-Za-z0-9_$.:/-]{0,95}$/;
export function namedAPI(error, name) {
  if (error && typeof error === 'object' && error.api === undefined && typeof name === 'string' && API_NAME.test(name)) error.api = name;
  return error;
}
// The API name a -32601 refusal is about, when the refusal names one. Read the
// tagged member, never the message text: a message can quote extension data.
export const refusedAPI = error => error?.code === -32601 && typeof error?.api === 'string' && API_NAME.test(error.api) ? error.api : undefined;
export function unsupported(name, reason = 'no corresponding negotiated octet contract') {
  throw rpcError(-32601, `unsupported_feature ${name}: ${reason}`);
}
// A refusal about a member this bridge or the host names itself, so the
// diagnostic can say which one (`unsupported_feature <name>: <reason>`).
export function unsupportedAPI(name, reason = 'no corresponding negotiated octet contract') {
  throw namedAPI(rpcError(-32601, `unsupported_feature ${name}: ${reason}`), name);
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
// An object handed to an extension callback with Pi's own plain-object shape
// (the `ctx`/`ui` facades, `pi.events`, event payloads). In Pi an optional
// member a factory probes is simply absent, so absence must read as absent:
// third-party factories detect features and other Pi releases this way
// (`ctx.goalStorageRoot !== undefined`, `pi.events[channel]`, an event field
// added by a later release). Members Pi 1.0.2 has but octet deliberately does
// not honor are registered at the facade as explicit refusing functions, so an
// unsupported *operation* still fails loudly.
export function facade(object, label, symbols) {
  return new Proxy(object, {
    get(target, key, receiver) {
      if (Reflect.has(target, key)) return Reflect.get(target, key, receiver);
      if (typeof key === 'symbol') return symbols?.get(key);
      return undefined;
    },
    set(target, key, value, receiver) {
      if (typeof key === 'symbol' && symbols && !Reflect.has(target, key)) { symbols.set(key, value); return true; }
      if (Reflect.has(target, key) || typeof key === 'symbol') return Reflect.set(target, key, value, receiver);
      // The key is the extension's own: it stays out of the notice.
      return unsupported(`${label}.${String(key)}`, 'the facade has no such member to assign');
    },
    deleteProperty(target, key) {
      if (typeof key === 'symbol' && symbols && !Reflect.has(target, key)) { symbols.delete(key); return true; }
      return Reflect.deleteProperty(target, key);
    },
    has: (target, key) => Reflect.has(target, key) || typeof key === 'symbol' && Boolean(symbols?.has(key)),
    ownKeys: target => [...new Set([...Reflect.ownKeys(target), ...symbols?.keys() ?? []])],
    getOwnPropertyDescriptor: (target, key) => Reflect.getOwnPropertyDescriptor(target, key) ??
      (typeof key === 'symbol' && symbols?.has(key) ? { value: symbols.get(key), writable: true, enumerable: true, configurable: true } : undefined),
  });
}
// An octet-built *data* object (an observed message, a native receipt, stream
// options). Here an absent member is a fact the host could not supply, and
// reading it must fail explicitly instead of looking like a real Pi value.
// When facts is supplied, only members of the Pi shape are such facts; other
// absent properties are ordinary JavaScript probes and read as undefined.
// The probed key is extension/data vocabulary, so this refusal never names it
// outside the process log (see namedAPI).
export function strict(object, label, facts) {
  return new Proxy(object, { get(target, key, receiver) {
    if (Reflect.has(target, key)) return Reflect.get(target, key, receiver);
    if (typeof key === 'symbol' || key === 'then' || key === 'toJSON') return undefined;
    if (facts && !facts.has(key)) return undefined;
    return unsupported(`${label}.${key}`);
  } });
}
export function plainJSON(value, name, max = 65536, { omitUndefined = false } = {}) {
  const visit = (v, depth) => {
    if (depth > 32) invalid(`${name} exceeds JSON depth`);
    if (v === null || typeof v === 'boolean' || typeof v === 'string') return;
    if (typeof v === 'number' && Number.isFinite(v) && (!Number.isInteger(v) || Number.isSafeInteger(v))) return;
    if (typeof v !== 'object' || (!Array.isArray(v) && ![Object.prototype, null].includes(Object.getPrototypeOf(v)))) invalid(`${name} must be plain JSON`);
    // Pi result details routinely contain optional undefined object members;
    // JSON persistence omits those. Arrays and every other JSON contract stay
    // strict: do not invent nulls or coerce unsupported values.
    for (const x of Object.values(v)) {
      if (omitUndefined && !Array.isArray(v) && x === undefined) continue;
      visit(x, depth + 1);
    }
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

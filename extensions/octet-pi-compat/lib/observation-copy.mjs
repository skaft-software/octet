// Copy containers, not immutable transcript strings. Pi's
// packages/agent/src/agent-loop.ts:434-450
// shallow-copies messages and shares their nested partial; keep that alias within
// an event, while retaining octet's stronger isolation across callbacks/events.
export function copyObservation(value, seen = new Map()) {
  if (value === null || typeof value !== 'object') return value;
  if (seen.has(value)) return seen.get(value);
  const result = Array.isArray(value) ? [] : {};
  seen.set(value, result);
  for (const key of Object.keys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    // Stream argument getters close over an immutable prefix. Materialize only
    // if a consumer actually reads it, never for an unrelated text/delta read.
    if (descriptor.get) {
      let ready = false, item;
      Object.defineProperty(result, key, { enumerable: true, configurable: true,
        get() { if (!ready) { item = copyObservation(descriptor.get()); ready = true; } return item; },
        set(value) { item = value; ready = true; } });
    } else Object.defineProperty(result, key, { value: copyObservation(value[key], seen), enumerable: true, configurable: true, writable: true });
  }
  return result;
}

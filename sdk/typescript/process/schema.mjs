// Deliberately bounded authoring subset, not a general JSON Schema engine.
const keywords = new Set(['type', 'properties', 'required', 'additionalProperties', 'items',
  'enum', 'description', 'title', 'minimum', 'maximum', 'minLength', 'maxLength', 'minItems', 'maxItems']);
const types = new Set(['object', 'array', 'string', 'number', 'integer', 'boolean', 'null']);
export const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
export const own = (value, key) => Object.hasOwn(value, key);

export function json(value, maxBytes = 1_048_575) {
  let nodes = 0;
  const visit = (v, depth) => {
    if (++nodes > 16_384 || depth > 32) throw new TypeError('JSON exceeds depth/node bound');
    if (v === null || typeof v === 'boolean') return;
    if (typeof v === 'string') {
      if (/\p{Surrogate}/u.test(v)) throw new TypeError('JSON contains a lone surrogate');
      return;
    }
    if (typeof v === 'number' && Number.isFinite(v) && (!Number.isInteger(v) || Number.isSafeInteger(v))) return;
    if (Array.isArray(v)) { for (const item of v) visit(item, depth + 1); return; }
    if (object(v) && [Object.prototype, null].includes(Object.getPrototypeOf(v))) {
      for (const [key, item] of Object.entries(v)) { visit(key, depth); visit(item, depth + 1); }
      return;
    }
    throw new TypeError('Value is not portable plain JSON');
  };
  visit(value, 0);
  const encoded = JSON.stringify(value);
  if (Buffer.byteLength(encoded) > maxBytes) throw new TypeError('JSON exceeds byte bound');
  return encoded;
}

export function schema(definition, root = true) {
  json(definition);
  const check = (s, depth) => {
    if (!object(s) || depth > 32) throw new TypeError('Schema must be a bounded object');
    for (const key of Object.keys(s)) if (!keywords.has(key)) throw new TypeError(`Unsupported schema keyword: ${key}`);
    if (!types.has(s.type)) throw new TypeError('Every schema node needs a supported type');
    for (const key of ['description', 'title']) if (own(s, key) && typeof s[key] !== 'string') throw new TypeError(`Invalid ${key}`);
    if (own(s, 'properties')) {
      if (s.type !== 'object' || !object(s.properties)) throw new TypeError('Invalid properties');
      for (const [key, child] of Object.entries(s.properties)) {
        if (Buffer.byteLength(key) > 256) throw new TypeError('Property name exceeds 256 bytes');
        check(child, depth + 1);
      }
    }
    if (own(s, 'required') && (s.type !== 'object' || !Array.isArray(s.required) ||
        s.required.some(key => typeof key !== 'string' || !own(s.properties ?? {}, key)) ||
        new Set(s.required).size !== s.required.length)) throw new TypeError('Invalid required properties');
    if (own(s, 'additionalProperties') && (s.type !== 'object' || typeof s.additionalProperties !== 'boolean')) throw new TypeError('additionalProperties must be boolean');
    if (s.type === 'array') {
      if (!own(s, 'items')) throw new TypeError('Array schema needs items');
      check(s.items, depth + 1);
    } else if (own(s, 'items')) throw new TypeError('items requires array type');
    if (own(s, 'enum') && (!Array.isArray(s.enum) || s.enum.length === 0 || s.enum.some(v => object(v) || Array.isArray(v) || !matches({type: s.type}, v)))) throw new TypeError('Invalid enum');
    for (const [low, high, kind] of [['minimum', 'maximum', 'number'], ['minLength', 'maxLength', 'string'], ['minItems', 'maxItems', 'array']]) {
      for (const key of [low, high]) if (own(s, key) &&
          ((kind === 'number' ? !['integer', 'number'].includes(s.type) : s.type !== kind) ||
          typeof s[key] !== 'number' || !Number.isFinite(s[key]) ||
          (kind !== 'number' && (!Number.isSafeInteger(s[key]) || s[key] < 0)))) throw new TypeError(`Invalid ${key}`);
      if (own(s, low) && own(s, high) && s[low] > s[high]) throw new TypeError('Inverted schema bounds');
    }
  };
  check(definition, 0);
  if (root && definition.type !== 'object') throw new TypeError('Tool parameters must have object type');
  return JSON.parse(JSON.stringify(definition));
}

export function matches(s, v) {
  const kind = s.type;
  if (kind === 'object' && !object(v) || kind === 'array' && !Array.isArray(v) ||
      kind === 'null' && v !== null || kind === 'integer' && !Number.isSafeInteger(v) ||
      kind === 'number' && (typeof v !== 'number' || !Number.isFinite(v)) ||
      ['string', 'boolean'].includes(kind) && typeof v !== kind) return false;
  if (s.enum && !s.enum.some(item => JSON.stringify(item) === JSON.stringify(v))) return false;
  if (['integer', 'number'].includes(kind) && (v < (s.minimum ?? -Infinity) || v > (s.maximum ?? Infinity))) return false;
  if (kind === 'string' && ([...v].length < (s.minLength ?? 0) || [...v].length > (s.maxLength ?? Infinity))) return false;
  if (kind === 'array' && (v.length < (s.minItems ?? 0) || v.length > (s.maxItems ?? Infinity) || s.items && !v.every(item => matches(s.items, item)))) return false;
  if (kind === 'object') {
    if ((s.required ?? []).some(key => !own(v, key))) return false;
    for (const [key, value] of Object.entries(v)) {
      if (own(s.properties ?? {}, key)) { if (!matches(s.properties[key], value)) return false; }
      else if (s.additionalProperties === false) return false;
    }
  }
  return true;
}

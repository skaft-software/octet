// Count compact JSON UTF-8 without materializing/cloning a history-sized string.
import { invalid } from './errors.mjs';
export function jsonBytes(value, limit) {
  let bytes = 0;
  const add = count => { bytes += count; if (bytes > limit) invalid('encoded document quota exceeded'); };
  const string = text => {
    add(2);
    for (const c of text) {
      const n = c.codePointAt(0);
      if (n >= 0xd800 && n <= 0xdfff) invalid('document contains an unpaired surrogate');
      add(n === 34 || n === 92 || [8, 9, 10, 12, 13].includes(n) ? 2 : n < 32 ? 6 : n < 128 ? 1 : n < 2048 ? 2 : n < 65536 ? 3 : 4);
    }
  };
  const visit = (v, depth) => {
    if (depth > 64) invalid('document depth');
    if (typeof v === 'string') { string(v); return; }
    if (v === null) { add(4); return; }
    if (typeof v === 'boolean') { add(v ? 4 : 5); return; }
    if (typeof v === 'number' && Number.isFinite(v) && (!Number.isInteger(v) || Number.isSafeInteger(v))) { add(JSON.stringify(v).length); return; }
    if (!v || typeof v !== 'object' || ![Object.prototype, null, Array.prototype].includes(Object.getPrototypeOf(v))) invalid('document must be inert JSON');
    add(2); let first = true;
    const member = (key, array) => {
      const d = Object.getOwnPropertyDescriptor(v, key);
      if (!d || !Object.hasOwn(d, 'value') || !d.enumerable) invalid('document member');
      if (!first) add(1); first = false;
      if (!array) { string(key); add(1); }
      visit(d.value, depth + 1);
    };
    if (Array.isArray(v)) {
      if (Reflect.ownKeys(v).length !== v.length + 1) invalid('sparse or extended document array');
      for (let i = 0; i < v.length; i++) member(String(i), true);
    } else for (const key of Reflect.ownKeys(v)) { if (typeof key !== 'string') invalid('document symbol'); member(key, false); }
  };
  visit(value, 0); return bytes;
}

// Pi builtin tool arguments -> octet native tool parameters.
//
// Pi 1.5 validates every tool call with pi-schema's `compile(schema, {coerce:
// true}).parse()`, so a model that sends `"path": 42` or `"timeout": "30"`
// reaches the builtin with `path: "42"` and `timeout: 30`; the call is valid.
// The translation below applies the same JSON-Schema coercion to the fields it
// reads before deciding anything, so a call Pi accepts is never refused here,
// and a call Pi refuses (a non-finite or out-of-range timeout, a path that is
// not a string even after coercion) fails with the same reason.
import { homedir } from 'node:os';
import { resolve } from 'node:path';
import { invalid, unsupported } from './errors.mjs';

// max_timeout in Pi 1.5 packages/coding-agent/src/core/tools/bash.ts.
const MAX_TIMEOUT_MS = 2147483647;

// pi-schema `coerce: true` string rule: a required string arrives as "null" for
// null, and numbers and booleans become their text form. `undefined` stays
// absent, which for `path` is Pi's own required-property failure.
const coerceString = value => (typeof value === 'string' ? value
  : typeof value === 'number' || typeof value === 'boolean' ? String(value)
  : value === null ? 'null'
  : undefined);
// pi-schema `coerce: true` number rule: numeric strings parse, booleans become
// 1/0, null becomes 0. `undefined` stays absent.
const coerceNumber = value => {
  if (typeof value === 'number') return value;
  if (typeof value === 'boolean') return value ? 1 : 0;
  if (value === null) return 0;
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
};

export function builtinArguments(name, input, cwd, workspace) {
  if (!input || typeof input !== 'object' || Array.isArray(input)) invalid('tool arguments');
  const base = cwd ?? workspace;
  if (typeof base !== 'string' || typeof workspace !== 'string') invalid('tool cwd');
  const effectiveCwd = resolve(workspace, base);
  if (['read', 'write', 'edit'].includes(name)) {
    const raw = coerceString(input.path);
    if (typeof raw !== 'string') invalid('tool path');
    const path = raw === '~' ? homedir() : raw.startsWith('~/') ? resolve(homedir(), raw.slice(2)) : resolve(effectiveCwd, raw);
    const translated = { ...input, path };
    if (name === 'read') {
      for (const key of ['offset', 'limit']) {
        if (input[key] === undefined) continue;
        const coerced = coerceNumber(input[key]);
        if (coerced === undefined) invalid(`tool ${key}`);
        translated[key] = coerced;
      }
      if (translated.limit === undefined) translated.limit = 2000;
    }
    return translated;
  }
  if (!['bash', 'powershell'].includes(name)) unsupported(`native ${name} tool`, 'no corresponding native builtin implementation');
  const timeout = input.timeout === undefined ? undefined : coerceNumber(input.timeout);
  if (input.timeout !== undefined && (timeout === undefined || !Number.isFinite(timeout) || timeout <= 0 || timeout * 1000 > MAX_TIMEOUT_MS)) invalid('tool timeout');
  const { timeout: _omitted, ...rest } = input;
  return { ...rest, cwd: effectiveCwd, ...(timeout === undefined ? {} : { timeout_ms: Math.ceil(timeout * 1000) }) };
}

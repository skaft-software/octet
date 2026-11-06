import { homedir } from 'node:os';
import { resolve } from 'node:path';
import { invalid, unsupported } from './errors.mjs';

export function builtinArguments(name, input, cwd, workspace) {
  if (!input || typeof input !== 'object' || Array.isArray(input)) invalid('tool arguments');
  const base = cwd ?? workspace;
  if (typeof base !== 'string' || typeof workspace !== 'string') invalid('tool cwd');
  const effectiveCwd = resolve(workspace, base);
  if (['read', 'write', 'edit'].includes(name)) {
    if (typeof input.path !== 'string') invalid('tool path');
    const path = input.path === '~' ? homedir() : input.path.startsWith('~/') ? resolve(homedir(), input.path.slice(2)) : resolve(effectiveCwd, input.path);
    return { ...input, path, ...(name === 'read' && input.limit === undefined ? { limit: 2000 } : {}) };
  }
  if (!['bash', 'powershell'].includes(name)) unsupported(`native ${name} tool`, 'no corresponding native builtin implementation');
  const { timeout, ...rest } = input;
  if (timeout !== undefined && (!Number.isFinite(timeout) || timeout <= 0 || timeout * 1000 > 2147483647)) invalid('tool timeout');
  return { ...rest, cwd: effectiveCwd, ...(timeout === undefined ? {} : { timeout_ms: Math.ceil(timeout * 1000) }) };
}

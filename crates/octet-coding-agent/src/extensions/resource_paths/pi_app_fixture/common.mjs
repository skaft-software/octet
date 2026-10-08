// Ordinary Pi factory fixture support: local files only, no wire peer or runtime shim.
import { appendFileSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const trace = fileURLToPath(new URL('./trace.jsonl', import.meta.url));
export function log(factory, kind, facts = {}) {
  appendFileSync(trace, JSON.stringify({ kind, factory, pid: process.pid, processCwd: process.cwd(), ...facts }) + '\n');
}
export function state(cwd) {
  const value = JSON.parse(readFileSync(join(cwd, 'resource-state.json'), 'utf8'));
  if (!['a', 'b', 'empty'].includes(value.phase)) throw new Error('invalid resource fixture phase');
  return value;
}
export function paths(factory, phase) {
  if (phase === 'empty') return factory === 'first' ? {} : undefined;
  return {
    skillPaths: [`  ./resources/${phase}/${factory}/skills  `],
    promptPaths: [`./resources/${phase}/${factory}/prompts`],
    ...(factory === 'first' ? { themePaths: [`./resources/${phase}/first/themes`] } : {}),
  };
}

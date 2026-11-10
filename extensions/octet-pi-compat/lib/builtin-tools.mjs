import { readFileSync } from 'node:fs';
import { builtinArguments } from './builtin-arguments.mjs';
import { fields, invalid, unsupported } from './errors.mjs';
import { executeHostTool } from './tool-host.mjs';
import { truncateHead, truncateTail } from './public-helpers.mjs';

const definitions = JSON.parse(readFileSync(new URL('../shims/pi-1.0.2/tool-definitions.json', import.meta.url), 'utf8'));
const fileTools = new Set(['read', 'write', 'edit']);
const shells = new Set(['bash', 'powershell']);

function projectBuiltinResult(name, result, output) {
  if (output === undefined || result.isError) return result;
  fields(output, ['value', 'duration_ms'], 'native builtin output');
  const value = output.value;
  if (name === 'read' && typeof value?.content === 'string') {
    const truncation = truncateHead(value.content);
    let text = truncation.content;
    const end = value.start_line + truncation.outputLines - 1;
    const next = truncation.truncated ? end + 1 : value.next_offset;
    if (next != null) text += `\n\n[Showing lines ${value.start_line}-${end} of ${value.total_lines}. Use offset=${next} to continue.]`;
    return { ...result, content: [{ type: 'text', text }],
      ...(truncation.truncated ? { details: { ...result.details, truncation } } : {}) };
  }
  if (shells.has(name) && typeof value?.stdout === 'string' && typeof value.stderr === 'string') {
    if (!Number.isInteger(value.exit_code) || !Number.isFinite(output.duration_ms) || output.duration_ms < 0) invalid('native shell output');
    const truncation = truncateTail(value.stdout + value.stderr);
    const structuredContent = { output: truncation.content, truncated: Boolean(truncation.truncated || value.truncated_stdout || value.truncated_stderr),
      exit_code: value.exit_code, wall_time_seconds: output.duration_ms / 1000 };
    return { ...result, content: [{ type: 'text', text: truncation.content }], structuredContent,
      ...(truncation.truncated ? { details: { ...result.details, truncation } } : {}) };
  }
  return result;
}

const shellQuote = value => `'${String(value).replaceAll("'", "'\\''")}'`;

// Pi 1.5.0 coding-agent dist/core/tools/bash.js:135-157 (resolveSpawnContext) and :160-175 (execute): the command is
// `commandPrefix\ncommand`, then `spawnHook({command, cwd, env})` returns the command, cwd and env to spawn with.
// Octet's host runs the shell, so command and cwd are applied directly; an env the hook changed is applied as
// shell exports/unsets in front of the command, which a bash child sees the same way as a spawn env.
function shellSpawn(name, options, input, cwd) {
  let command = typeof input?.command === 'string' ? input.command : '';
  if (options.commandPrefix !== undefined) command = `${options.commandPrefix}\n${command}`;
  if (options.spawnHook === undefined) return { input: { ...input, command }, cwd };
  const base = { ...process.env };
  const resolved = options.spawnHook({ command, cwd, env: { ...base } });
  if (!resolved || typeof resolved !== 'object' || typeof resolved.command !== 'string' || typeof resolved.cwd !== 'string'
      || !resolved.env || typeof resolved.env !== 'object') invalid('spawnHook result');
  const lines = [];
  for (const [key, value] of Object.entries(resolved.env)) {
    if (value === base[key] || value === undefined) continue;
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) invalid('spawnHook env name');
    lines.push(`export ${key}=${shellQuote(value)}`);
  }
  for (const key of Object.keys(base)) if (!Object.hasOwn(resolved.env, key) || resolved.env[key] === undefined) {
    if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) lines.push(`unset ${key}`);
  }
  if (lines.length && name !== 'bash') unsupported('powershell spawnHook env', 'environment changes are applied to bash only');
  return { input: { ...input, command: lines.length ? `${lines.join('\n')}\n${resolved.command}` : resolved.command }, cwd: resolved.cwd };
}

export function createBuiltinDefinition(name, cwd, options = {}) {
  if (!Object.hasOwn(definitions, name)) invalid('builtin tool name');
  fields(options, name === 'read' ? ['autoResizeImages', 'resizeOptions'] : shells.has(name) ? ['commandPrefix', 'spawnHook'] : [], `${name} tool options`);
  if (options.commandPrefix !== undefined && typeof options.commandPrefix !== 'string') invalid(`${name} commandPrefix`);
  if (options.spawnHook !== undefined && typeof options.spawnHook !== 'function') invalid(`${name} spawnHook`);
  if (options.autoResizeImages !== undefined && options.autoResizeImages !== true) unsupported('read.autoResizeImages', 'image processing remains native-owned');
  if (options.resizeOptions !== undefined) unsupported('read.resizeOptions', 'image processing remains native-owned');
  if (cwd !== undefined && typeof cwd !== 'string') invalid('tool cwd');
  const definition = structuredClone(definitions[name]);
  if (name === 'edit') definition.prepareArguments = input => {
    if (!input || typeof input !== 'object' || Array.isArray(input)) return input;
    const result = { ...input };
    if (typeof result.edits === 'string') { try { result.edits = JSON.parse(result.edits); } catch {} }
    if (result.edits && !Array.isArray(result.edits) && typeof result.edits.oldText === 'string' && typeof result.edits.newText === 'string') result.edits = [result.edits];
    if (typeof result.oldText === 'string' && typeof result.newText === 'string') {
      result.edits = [...(Array.isArray(result.edits) ? result.edits : []), { oldText: result.oldText, newText: result.newText }];
      delete result.oldText; delete result.newText;
    }
    return result;
  };
  definition.execute = async (toolCallId, args, signal, onUpdate, context) => {
    if (shells.has(name) && args?.timeout !== undefined && (!Number.isFinite(args.timeout) || args.timeout <= 0 || args.timeout * 1000 > 2147483647)) invalid('tool timeout');
    if (!fileTools.has(name) && !shells.has(name)) unsupported(`native ${name} tool`, 'no corresponding native builtin implementation');
    const result = await executeHostTool(name, args, { builtin: true, toolCallId, signal, onUpdate,
      translate: (input, workspace) => {
        const spawn = shells.has(name) ? shellSpawn(name, options, input, context?.cwd ?? cwd) : { input, cwd: context?.cwd ?? cwd };
        return builtinArguments(name, spawn.input, spawn.cwd, workspace);
      },
      project: (value, output) => projectBuiltinResult(name, value, output),
    });
    if (result.isError && !shells.has(name)) throw new Error(result.content.filter(part => part.type === 'text').map(part => part.text).join('\n'));
    return result;
  };
  return definition;
}

export const createReadToolDefinition = (cwd, options) => createBuiltinDefinition('read', cwd, options);
export const createEditToolDefinition = (cwd, options) => createBuiltinDefinition('edit', cwd, options);
export const createWriteToolDefinition = (cwd, options) => createBuiltinDefinition('write', cwd, options);
export const createBashToolDefinition = (cwd, options) => createBuiltinDefinition('bash', cwd, options);
export const createPowerShellToolDefinition = (cwd, options) => createBuiltinDefinition('powershell', cwd, options);
export const createGrepToolDefinition = (cwd, options) => createBuiltinDefinition('grep', cwd, options);
export const createFindToolDefinition = (cwd, options) => createBuiltinDefinition('find', cwd, options);
export const createLsToolDefinition = (cwd, options) => createBuiltinDefinition('ls', cwd, options);

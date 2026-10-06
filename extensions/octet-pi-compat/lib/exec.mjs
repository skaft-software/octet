// pi.exec delegates execution and authorization to Rust, never node:child_process.
import { bounded, fields, invalid, strict } from './errors.mjs';

export async function exec(runtime, store, command, args, options = {}) {
  runtime.require('process_exec_v1'); runtime.assertOwner(store);
  fields(options, ['cwd', 'timeout', 'signal'], 'exec options');
  bounded(command, 'exec command', 131072, { controls: true });
  if (!command || command.includes('\0')) invalid('exec command');
  if (!Array.isArray(args) || args.length > 256) invalid('exec args');
  for (const arg of args) { bounded(arg, 'exec argument', 131072, { controls: true }); if (arg.includes('\0')) invalid('exec argument NUL'); }
  if (options.timeout !== undefined && (!Number.isSafeInteger(options.timeout) || options.timeout < 0)) invalid('exec timeout');
  if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) invalid('exec signal');
  const cwd = options.cwd ?? store.state.workspace;
  bounded(cwd, 'exec cwd', 4096, { controls: true });
  if (cwd.includes('\0')) invalid('exec cwd NUL');
  // The existing serialized transport allocates the child ID synchronously.
  // Cancellation asks Rust to stop execution but keeps the result request live,
  // so Pi receives real partial stdout/stderr and killed instead of a fake result.
  const work = runtime.hostCall('process/exec', { resource_owner: store.state.owner,
    command, args, cwd, timeout_ms: options.timeout || null, cancelled: options.signal?.aborted ?? false }, store);
  const id = `pi:${runtime.transport.childId}`;
  const cancel = () => runtime.transport.notify('process/exec/cancel', { id }).catch(error => runtime.backgroundError(error));
  options.signal?.addEventListener('abort', cancel, { once: true });
  try {
    // This is Pi's awaited result, not a void setter. A caller may catch a
    // native refusal; do not replay that handled rejection at command flush.
    const result = await work;
    fields(result, ['stdout', 'stderr', 'code', 'killed'], 'exec result');
    bounded(result.stdout, 'exec stdout', 1048576, { controls: true }); bounded(result.stderr, 'exec stderr', 1048576, { controls: true });
    if (!Number.isSafeInteger(result.code) || typeof result.killed !== 'boolean') invalid('exec result');
    return strict(result, 'exec result');
  } finally { options.signal?.removeEventListener('abort', cancel); }
}

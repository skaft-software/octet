// ctx.compact is synchronous void. A live origin must finish writing its reply
// AND settle its ordinary children before this retained request can be sent.
import { bounded, fields, invalid, rpcError, strict, unsupported } from './errors.mjs';

const domains = new WeakMap(), MAX_COMPACTIONS = 8;
const recursive = store => ['session_before_compact', 'session_compact'].includes(store?.hook);
const cancelled = () => rpcError(-32800, 'request cancelled');
function domain(runtime) {
  let value = domains.get(runtime);
  if (!value) { value = { jobs: new Set(), outcomes: new WeakMap() }; domains.set(runtime, value); }
  return value;
}
function available(runtime, store) {
  if (runtime.stopping || runtime.transport.closed) throw rpcError(-32002, 'host is draining or unavailable');
  runtime.assertOwner(store); store.controller.signal.throwIfAborted();
}
function receipt(value) {
  fields(value, ['entry_id', 'summary', 'first_kept'], 'compaction receipt');
  if (!bounded(value.entry_id, 'compaction entry id', 256) || !bounded(value.first_kept, 'compaction first kept entry', 256)) invalid('compaction receipt entry identity');
  return strict({ summary: bounded(value.summary, 'compaction summary', 262144, { controls: true }), firstKeptEntryId: value.first_kept }, 'compaction result');
}

// Runtime.track already diagnoses retained setter failures. Remember those errors
// so returning/awaiting that same setter from a callback does not diagnose twice.
class CallbackWork extends Set {
  reported = new Set();
  add(promise) { promise.catch(error => { this.reported.add(error); }); return super.add(promise); }
}
function release(runtime, job) {
  domain(runtime).jobs.delete(job);
  job.origin.controller.signal.removeEventListener('abort', job.abort);
}
async function callback(runtime, job, handler, value) {
  if (!handler) return;
  const store = job.store;
  try { await runtime.scope.run(store, () => handler(value)); }
  catch (error) { if (!store.pending.reported.has(error)) runtime.backgroundError(error); }
  // This work belongs to the retained callback, never to origin.pending. Track
  // diagnoses its failures; waiting here only bounds the callback's owner slot.
  while (store.pending.size) await Promise.allSettled([...store.pending]);
}
function finish(runtime, job, error, value) {
  if (job.phase === 'callback' || job.phase === 'done') return;
  job.phase = 'callback';
  const work = error === undefined
    ? callback(runtime, job, job.options.onComplete, value)
    : job.options.onError ? callback(runtime, job, job.options.onError, error) : Promise.resolve(runtime.backgroundError(error));
  // Callback failures are diagnostics only: never feed onComplete's exception
  // into onError, and never invoke onError recursively if it throws.
  work.finally(() => { job.phase = 'done'; release(runtime, job); }).catch(error => runtime.backgroundError(error));
}
function stop(runtime, job, error) {
  job.store.controller.abort(error);
  const live = runtime.active.get(job.origin.id);
  // Even on failure, callbacks must not add work to a still-settling handler.
  if (job.phase === 'queued' && live?.controller !== job.origin.controller) finish(runtime, job, error);
}
async function submit(runtime, job) {
  if (job.phase !== 'queued') return;
  job.phase = 'request';
  try {
    available(runtime, job.store);
    const result = await runtime.hostCall('session/compact', job.options.customInstructions === undefined ? {} : { custom_instructions: job.options.customInstructions }, job.store);
    available(runtime, job.store);
    finish(runtime, job, undefined, receipt(result));
  } catch (error) { finish(runtime, job, error); }
}

export function requestCompaction(runtime, store, options = {}) {
  const allowed = ['customInstructions', 'onComplete', 'onError'];
  fields(options, allowed, 'ctx.compact options');
  if (![Object.prototype, null].includes(Object.getPrototypeOf(options))) invalid('ctx.compact options must be plain');
  for (const key of Reflect.ownKeys(options)) if (!allowed.includes(key)) unsupported(`ctx.compact options.${String(key)}`, 'option would not be honored');
  const { customInstructions, onComplete, onError } = options;
  if (customInstructions !== undefined) bounded(customInstructions, 'compaction customInstructions', 16384);
  for (const [name, value] of [['onComplete', onComplete], ['onError', onError]]) if (value !== undefined && typeof value !== 'function') invalid(`ctx.compact ${name} must be a function`);
  runtime.require('session_control_v1'); runtime.require('session_compaction_v1');
  if (recursive(store) || recursive(runtime.scope.getStore())) unsupported('ctx.compact', 'recursive compaction from session_before_compact/session_compact is not supported');
  available(runtime, store);
  if (!Number.isSafeInteger(store.id) || store.id < 0) throw rpcError(-32002, 'active numeric parent_request_id required');
  const state = domain(runtime), failure = state.outcomes.get(store.controller);
  if (failure !== undefined) throw failure;
  if ([...state.jobs].some(job => job.store.state === store.state)) throw rpcError(-32012, 'bounds_exceeded outstanding compaction for owner');
  if (state.jobs.size >= MAX_COMPACTIONS) throw rpcError(-32012, 'bounds_exceeded outstanding compactions');
  const retained = { id: store.id, state: store.state, factory: store.factory, method: 'session/compact', hook: store.hook,
    compactionOrigin: store.controller, controller: new AbortController(), live: false, pending: new CallbackWork(), errors: [] };
  const job = { origin: store, store: retained, options: { customInstructions, onComplete, onError }, phase: 'queued' };
  job.abort = () => stop(runtime, job, store.controller.signal.reason ?? cancelled());
  state.jobs.add(job); store.controller.signal.addEventListener('abort', job.abort, { once: true });
  const live = runtime.active.get(store.id);
  if (!live || live.controller !== store.controller) void submit(runtime, job);
  return undefined;
}

// Captured ctx methods still pass their original store explicitly. Only while
// executing its own compaction callback, route their calls/tracking to the new
// retained store; preserve factory provenance and never redirect another owner.
export function compactionCallbackStore(runtime, store) {
  const callback = runtime.scope.getStore();
  return callback?.compactionOrigin && callback.compactionOrigin === store?.controller && callback.state === store.state && callback.id === store.id
    ? { ...callback, factory: store.factory } : store;
}

// Called AFTER successful/error reply writing and transport.settleParent, never
// as part of dispatch/flush. Remember failures by controller, not reusable ids.
export function settleCompactions(runtime, store, error = undefined) {
  const state = domain(runtime);
  error = store.controller.signal.aborted ? store.controller.signal.reason ?? cancelled() : error;
  state.outcomes.set(store.controller, error);
  for (const job of [...state.jobs]) if (job.origin.controller === store.controller && job.phase === 'queued') {
    if (error !== undefined) stop(runtime, job, error);
    else void submit(runtime, job);
  }
}
export function cancelCompactions(runtime, id) {
  for (const job of domains.get(runtime)?.jobs ?? []) if (job.origin.id === id) {
    // Retained origin callbacks must also remain cancelled after this job ends.
    job.origin.controller.abort(cancelled());
  }
}
export function retireCompactions(runtime, state = undefined, error = rpcError(-32002, 'not_foreground_owner compaction owner retired')) {
  for (const job of domains.get(runtime)?.jobs ?? []) if (!state || job.store.state === state) stop(runtime, job, error);
}

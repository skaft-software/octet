import { basename, dirname } from 'node:path';
import { performance } from 'node:perf_hooks';
import { nativeTimer } from './timers.mjs';
import { tracePhase, traceEnabled } from './startup-trace.mjs';

// Wall-clock budgets for factory handlers.
//
// Every hook the host sends runs on one ordered queue, and each hook runs its
// factories' handlers one after another. A single handler that never settles
// (a network call, a child process or an MCP connection that never answers)
// would otherwise hold that queue forever: the host's own deadline fires, the
// prompt fails and resource discovery refuses, while the bridge stays wedged.
//
// A handler that outlives its budget is detached instead: the hook stops
// waiting for it, the handler keeps running, the factory is reported once by
// name, and the next hook proceeds. A detached handler that is still running
// is skipped on later hooks rather than invoked again, so a stall costs one
// budget, not one per prompt. Handlers that can veto (tool guards) fail closed:
// a guard that cannot answer in time blocks the call, never lets it through.
//
// The totals keep the whole chain under the host's own deadlines for the same
// hook (before_prompt: 5 s).

const PER_HANDLER_MS = { before_agent_start: 2000, input: 2000, tool_call: 10000 };
const TOTAL_MS = { before_agent_start: 4000, input: 4000, tool_call: 10000 };
const DEFAULT_PER_HANDLER_MS = 5000;
const DEFAULT_TOTAL_MS = 15000;
const MIN_WAIT_MS = 250;
const TIMED_OUT = Symbol('handler budget exceeded');

// The suite shortens every budget; the variable never reaches a bridge started
// by the host, which passes an allow-listed environment only.
function forcedBudget() {
  const value = Number(process.env.OCTET_PI_HANDLER_BUDGET_MS);
  return Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

export function budgetFor(event) {
  const forced = forcedBudget();
  return {
    perHandler: forced ?? PER_HANDLER_MS[event] ?? DEFAULT_PER_HANDLER_MS,
    total: forced ? forced * 4 : TOTAL_MS[event] ?? DEFAULT_TOTAL_MS,
  };
}

// A factory's own async setup is awaited while the bridge loads, ahead of every
// hook, and the host waits 30 s for the whole load (Node start, adapter and
// every factory). One setup that never settles would cost all of it and take
// the mirror down, so each gets a share and the rest of the setup carries on.
const FACTORY_SETUP_MS = 10000;
const FACTORY_SETUP_TOTAL_MS = 20000;

export class LoadBudget {
  constructor() {
    const forced = forcedBudget();
    this.perFactory = forced ?? FACTORY_SETUP_MS;
    this.total = forced ? forced * 4 : FACTORY_SETUP_TOTAL_MS;
    this.started = performance.now();
  }

  // Resolves with the setup's value and rethrows its own failure. A setup that
  // outlives its share rejects with a `FactoryStalled` error; it keeps running,
  // but the caller drops everything it registered and refuses what it adds later.
  // The share covers the module import as well as the factory body: a top-level
  // await that never settles stalls the load just the same.
  async settle(call) {
    const left = this.total - (performance.now() - this.started);
    const waited = Math.max(Math.min(MIN_WAIT_MS, this.perFactory), Math.min(this.perFactory, left));
    const timer = nativeTimer(waited);
    const setup = Promise.resolve().then(call);
    setup.catch(() => {}); // a setup left running may still fail; nobody waits for it
    try {
      const outcome = await Promise.race([setup.then(value => ({ value })), timer.promise.then(() => TIMED_OUT)]);
      if (outcome === TIMED_OUT) throw Object.assign(new Error(`setup did not finish within ${Math.round(waited / 100) / 10} s`), { name: 'FactoryStalled' });
      return outcome.value;
    } finally { timer.cancel(); }
  }
}

export class HookBudget {
  constructor(runtime, store, event) {
    this.runtime = runtime; this.store = store; this.event = event;
    ({ perHandler: this.perHandler, total: this.total } = budgetFor(event));
    this.started = performance.now();
    this.detached = new Map(); // factory path -> milliseconds actually waited
  }

  // What this handler may still spend: its own budget, capped by what is left
  // of the hook's total, but never so little that a healthy handler is starved.
  wait() {
    const left = this.total - (performance.now() - this.started);
    return Math.max(Math.min(MIN_WAIT_MS, this.perHandler), Math.min(this.perHandler, left));
  }

  describe(entry) {
    return entry.factory === undefined ? entry.label : this.runtime.config.extensions[entry.factory] ?? String(entry.factory);
  }

  // Run one handler. Resolves { ran: true, value } when it settled in time; a
  // failure rethrows exactly as an unbudgeted handler would. Resolves
  // { ran: false, busy } when the handler was skipped because an earlier call
  // is still running, or detached because it outlived its budget.
  async run(entry, child, call) {
    if (entry.busy) {
      this.trace(entry, 0, 'skipped-busy');
      return { ran: false, busy: true };
    }
    const { store } = this;
    const started = performance.now();
    const pendingBefore = new Set(store.pending), errorsBefore = store.errors.length;
    const waited = this.wait();
    const timer = nativeTimer(waited);
    const settled = Promise.resolve().then(call).then(value => ({ value }), error => ({ error }));
    entry.busy = true;
    settled.then(late => {
      entry.busy = false;
      if (child.detached) {
        this.trace(entry, performance.now() - started, late.error ? 'late-failed' : 'late-settled');
        if (late.error && entry.factory !== undefined) this.runtime.reportCallbackError(this.event, entry.factory, late.error);
      }
    });
    let outcome;
    try { outcome = await Promise.race([settled, timer.promise.then(() => TIMED_OUT)]); }
    finally { timer.cancel(); }
    if (outcome === TIMED_OUT) {
      // Work the handler already queued on this hook must not hold the hook's
      // own flush, and anything it queues later is observed but never joined.
      child.detached = true;
      for (const pending of [...store.pending]) if (!pendingBefore.has(pending)) store.pending.delete(pending);
      store.errors.length = errorsBefore;
      this.detached.set(this.describe(entry), waited);
      this.trace(entry, performance.now() - started, 'detached');
      return { ran: false, busy: false };
    }
    this.trace(entry, performance.now() - started, outcome.error ? 'failed' : 'ok');
    if (outcome.error) throw outcome.error;
    return { ran: true, value: outcome.value };
  }

  // Off-screen attribution on the shared OCTET_STARTUP_TRACE channel: which
  // factory spent how long in which handler.
  trace(entry, milliseconds, outcome) {
    if (!traceEnabled) return;
    const path = this.describe(entry);
    const name = entry.factory === undefined ? path : `${basename(dirname(path))}/${basename(path)}`;
    tracePhase('pi-compat.handler', `event=${this.event} factory=${name} took=${Math.round(milliseconds)}ms outcome=${outcome}`);
  }

  // One report per factory and event, the first time a handler is left running.
  finish() {
    if (this.detached.size) this.runtime.issues.slow(this.event, [...this.detached]);
  }
}

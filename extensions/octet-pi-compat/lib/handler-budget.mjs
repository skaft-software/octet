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
// The callback-wait totals leave time before the host's own deadlines for
// before_prompt and resources_discover (5 s). They do not bound queueing,
// transport, synchronous JS or a later flush of tracked host operations.

const PER_HANDLER_MS = { before_agent_start: 2000, input: 2000, resources_discover: 4000, tool_call: 10000 };
const TOTAL_MS = { before_agent_start: 4000, input: 4000, resources_discover: 4000, tool_call: 10000 };
const DEFAULT_PER_HANDLER_MS = 5000;
const DEFAULT_TOTAL_MS = 15000;
const MIN_WAIT_MS = 250;
// A tool guard may ask the user (a permission dialog) and the user takes as long
// as they take, as in Pi. That time is not charged to the guard, up to a cap kept
// below the host's 30 s request timeout, so a guard whose dialog never settles
// still fails closed. before_agent_start is not listed: the host's 5 s prompt
// deadline applies to it regardless.
const USER_WAIT_EVENTS = new Set(['tool_call']);
const USER_WAIT_CAP_MS = 20000;
const TIMED_OUT = Symbol('handler budget exceeded');
const PREEMPTED = Symbol('hook preempted by quit');

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
// The minimum wait keeps a healthy setup reached late from being starved, but
// all of those floors together draw on one fixed reserve, so any number of
// stalled factories ends the load by total + reserve, inside the host's 30 s.
const FACTORY_SETUP_MS = 10000;
const FACTORY_SETUP_TOTAL_MS = 20000;
const FACTORY_SETUP_FLOOR_RESERVE_MS = 2000;

export class LoadBudget {
  constructor() {
    const forced = forcedBudget();
    this.perFactory = forced ?? FACTORY_SETUP_MS;
    this.total = forced ? forced * 4 : FACTORY_SETUP_TOTAL_MS;
    this.deadline = this.total + (forced ?? FACTORY_SETUP_FLOOR_RESERVE_MS);
    this.started = performance.now();
  }

  // This setup's share: its own budget, capped by what is left of the total,
  // floored for a late healthy setup, and never past the absolute deadline.
  wait() {
    const elapsed = performance.now() - this.started;
    const share = Math.max(Math.min(MIN_WAIT_MS, this.perFactory), Math.min(this.perFactory, this.total - elapsed));
    return Math.max(0, Math.min(share, this.deadline - elapsed));
  }

  // Resolves with the setup's value and rethrows its own failure. A setup that
  // outlives its share rejects with a `FactoryStalled` error; it keeps running,
  // but the caller drops everything it registered and refuses what it adds later.
  // The share covers the module import as well as the factory body: a top-level
  // await that never settles stalls the load just the same.
  async settle(call) {
    const waited = this.wait();
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

// Time one handler spends awaiting a dialog the user answers. `ctx.ui` marks
// those waits through `child.userWait` while the budget runs the handler.
export class UserWait {
  constructor() { this.active = 0; this.since = 0; this.total = 0; this.release = undefined; }

  track(promise) {
    if (this.active++ === 0) this.since = performance.now();
    const end = () => {
      if (--this.active > 0) return;
      this.total += performance.now() - this.since;
      this.release?.(); this.release = undefined;
    };
    promise.then(end, end);
    return promise;
  }

  spent() {
    return Math.min(USER_WAIT_CAP_MS, this.total + (this.active ? performance.now() - this.since : 0));
  }

  // Resolves when no dialog is open or the cap is reached.
  idle() {
    const room = USER_WAIT_CAP_MS - this.spent();
    if (!this.active || room <= 0) return Promise.resolve();
    const timer = nativeTimer(room);
    return Promise.race([new Promise(resolve => { this.release = resolve; }), timer.promise]).finally(() => timer.cancel());
  }
}

export class HookBudget {
  constructor(runtime, store, event) {
    this.runtime = runtime; this.store = store; this.event = event;
    ({ perHandler: this.perHandler, total: this.total } = budgetFor(event));
    this.started = performance.now();
    this.detached = new Map(); // factory path -> milliseconds actually waited
    this.excluded = 0; // milliseconds the hook's handlers spent awaiting the user
  }

  // Keep a small shared reserve for healthy handlers reached late, but never
  // let their individual floors extend the hook's absolute total deadline.
  wait() {
    const left = this.total - (performance.now() - this.started - this.excluded);
    const reserve = Math.min(MIN_WAIT_MS, this.perHandler);
    const share = Math.max(reserve, Math.min(this.perHandler, left - reserve));
    return Math.max(0, Math.min(left, share));
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
    const waited = this.wait();
    if (waited === 0) {
      this.trace(entry, 0, 'skipped-budget');
      return { ran: false, busy: false };
    }
    const { store } = this;
    const started = performance.now();
    const pendingBefore = new Set(store.pending);
    const userWait = USER_WAIT_EVENTS.has(this.event) ? child.userWait = new UserWait() : undefined;
    const settled = Promise.resolve().then(call).then(value => ({ value }), error => ({ error }));
    entry.busy = true;
    settled.then(late => {
      entry.busy = false;
      if (child.detached) {
        this.trace(entry, performance.now() - started, late.error ? 'late-failed' : 'late-settled');
        if (late.error && entry.factory !== undefined) this.runtime.reportCallbackError(this.event, entry.factory, late.error);
      }
    });
    // A quit aborts the hook still starting up: stop waiting so shutdown handlers run at
    // once, and let the handler finish in the background. Nothing is reported, the
    // process is going away.
    const { signal } = store.controller;
    let release;
    const preempted = new Promise(resolve => {
      release = () => { if (store.preempted) resolve(PREEMPTED); };
      if (signal.aborted) release(); else signal.addEventListener('abort', release, { once: true });
    });
    let outcome;
    try {
      for (;;) {
        const left = started + waited + (userWait?.spent() ?? 0) - performance.now();
        if (userWait?.active && userWait.spent() < USER_WAIT_CAP_MS) {
          outcome = await Promise.race([settled, userWait.idle().then(() => TIMED_OUT), preempted]);
          if (outcome !== TIMED_OUT) break;
          continue;
        }
        if (left <= 0) { outcome = TIMED_OUT; break; }
        const timer = nativeTimer(left);
        try { outcome = await Promise.race([settled, timer.promise.then(() => TIMED_OUT), preempted]); }
        finally { timer.cancel(); }
        if (outcome !== TIMED_OUT) break;
      }
    } finally { signal.removeEventListener('abort', release); }
    this.excluded += userWait?.spent() ?? 0;
    if (outcome === PREEMPTED) {
      child.detached = true;
      for (const pending of [...store.pending]) if (!pendingBefore.has(pending)) store.pending.delete(pending);
      this.trace(entry, performance.now() - started, 'preempted');
      return { ran: false, busy: false };
    }
    if (outcome === TIMED_OUT) {
      // Work the handler already queued on this hook must not hold the hook's
      // own flush, and anything it queues later is observed but never joined.
      child.detached = true;
      for (const pending of [...store.pending]) if (!pendingBefore.has(pending)) store.pending.delete(pending);
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

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
// Pi itself has no per-handler hook timeout, and this bridge is a Pi
// compatibility layer: the prompt phases (`input` and `before_agent_start`) run
// each handler until it settles, however long the dialog, host query or nested
// call it waits on takes, and its result is applied exactly as Pi applies it. A
// handler that settles after its run was cancelled can no longer change that
// prompt, so it is reported once by name (see `late`), never dropped in silence.
//
// The budgets that remain cover the phases where a stall has nothing to give:
// the context phases and resources discovery must fit inside the host's 5 s
// deadline, and a tool guard that cannot answer in time must fail closed. There a
// handler that outlives its budget is detached instead: the hook stops waiting
// for it, the handler keeps running, the factory is reported once by name, and
// the next hook proceeds. A detached handler that is still running is skipped on
// later hooks rather than invoked again, so a stall costs one budget, not one per
// prompt.
//
// The callback-wait totals leave time before the host's own deadlines for
// before_prompt, provider_context and resources_discover (5 s). They do not
// bound queueing, transport, synchronous JS or a later flush of tracked host
// operations. Both provider-context phases share the same total wait budget.

const PER_HANDLER_MS = { context: 2000, context_with_system: 2000, resources_discover: 4000, tool_call: 10000 };
const TOTAL_MS = { context: 4000, context_with_system: 4000, resources_discover: 4000, tool_call: 10000 };
// Pi runs these hooks until the handler settles, so no handler is detached from
// them and none is reported as "skipped this time": `input` and
// `before_agent_start` compose the run, `session_start` announces it. Cancel
// and preemption still stop the wait at once. The phases below keep a wait
// because they must answer inside a host deadline, and a tool guard must fail
// closed; their user-dialog time is excluded separately.
const UNBUDGETED_EVENTS = new Set(['input', 'before_agent_start', 'session_start']);
const DEFAULT_PER_HANDLER_MS = 5000;
const DEFAULT_TOTAL_MS = 15000;
const MIN_WAIT_MS = 250;
// A tool guard may ask the user (a permission dialog) and the user takes as long
// as they take, as in Pi. That time is not charged to the guard, up to a cap kept
// below the host's 30 s request timeout, so a guard whose dialog never settles
// still fails closed.
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

// Undefined for a phase Pi leaves unbudgeted: there is no wait to hand out.
export function budgetFor(event) {
  if (UNBUDGETED_EVENTS.has(event)) return undefined;
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
  // `budget` is undefined for the phases Pi leaves unbudgeted; the runner then
  // only has to notice a cancelled or preempted run.
  constructor(runtime, store, event, started = performance.now()) {
    this.runtime = runtime; this.store = store; this.event = event;
    this.budget = budgetFor(event);
    this.perHandler = this.budget?.perHandler; this.total = this.budget?.total;
    this.started = started;
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

  // A handler that settled after its hook stopped waiting still answered. Pi
  // would have applied that answer to the run it was composing; this hook has
  // already answered the host, so name what was lost — once, by extension.
  late(entry, what) {
    const issues = this.runtime.issues, path = this.describe(entry);
    if (this.runtime.stopping || typeof issues?.publish !== 'function') return;
    if (typeof issues.firstEventIssue === 'function' && !issues.firstEventIssue(`late:${this.event}`, path)) return;
    issues.publish([{ path, level: 'warning',
      reason: `${String(this.event).slice(0, 128)} ${what} arrived after the hook returned and was not applied`,
      next: 'Update the extension so it answers without a long wait, or disable it.' }]);
  }

  // An answer that arrived too late for its hook. A quit is shutting the bridge
  // down, so nothing is reported then; otherwise the error is the callback
  // failure it would have been, and a result is named by the event's own
  // `onLateResult` (or generically) instead of vanishing.
  reportLate(entry, late, started, options) {
    if (this.store.preempted) return;
    this.trace(entry, performance.now() - started, late.error ? 'late-failed' : 'late-settled');
    if (late.error) {
      if (entry.factory !== undefined) this.runtime.reportCallbackError(this.event, entry.factory, late.error, this.store.state);
      return;
    }
    if (late.value === undefined) return;
    let what;
    try { what = options.onLateResult ? options.onLateResult(entry, late.value) : 'result'; }
    catch { what = 'result'; }
    if (what) this.late(entry, what);
  }

  // Run one handler and hand back its answer. A failure rethrows exactly as the
  // handler threw, and the caller reports it and continues with the next one, as
  // Pi does. Resolves { ran: true, value } when the handler answered, or
  // { ran: false, busy } when a budgeted handler is still running from an earlier
  // hook (skipped, never re-entered), or the run was preempted by a quit.
  // `options.onLateResult(entry, value)` names what a value would have changed,
  // for the answer that can no longer be applied.
  async run(entry, child, call, options = {}) {
    const { budget, store } = this;
    // Only a budgeted handler can be left running by an earlier hook; Pi's own
    // prompt phases are entered again on the next prompt.
    if (budget && entry.busy) {
      this.trace(entry, 0, 'skipped-busy');
      return { ran: false, busy: true };
    }
    const waited = budget ? this.wait() : 0;
    if (budget && waited === 0) {
      this.trace(entry, 0, 'skipped-budget');
      if (entry.factory !== undefined && ['context', 'context_with_system'].includes(this.event)) {
        this.runtime.reportCallbackError(this.event, entry.factory, new Error('context budget exhausted'), store.state);
      }
      return { ran: false, busy: false };
    }
    const started = performance.now();
    const pendingBefore = new Set(store.pending);
    const userWait = USER_WAIT_EVENTS.has(this.event) ? child.userWait = new UserWait() : undefined;
    const settled = Promise.resolve().then(call).then(value => ({ value }), error => ({ error }));
    if (budget) entry.busy = true;
    settled.then(late => {
      if (budget) entry.busy = false;
      // The hook stopped waiting for this one: detached out of its budget,
      // preempted by a quit, or a run the host cancelled.
      if (!child.detached && !store.controller.signal.aborted) return;
      this.reportLate(entry, late, started, options);
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
      if (!budget) outcome = await Promise.race([settled, preempted]);
      else for (;;) {
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
    if (budget) this.excluded += userWait?.spent() ?? 0;
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
    if (this.detached.size) this.runtime.issues.slow(this.event, [...this.detached], this.store.state);
  }
}

import { performance } from 'node:perf_hooks';
import { invalid } from './errors.mjs';

export const LIVENESS_FEATURE = 'dispatcher_liveness_v1';

// Pi invokes factories/handlers in-process and awaits input and UI answers:
// packages/coding-agent/src/core/extensions/runner.ts:1190-1233,1337-1360,
// 580-614,1525-1574. Our child process needs dispatcher progress, not evidence
// that its independent pipe worker can still read. No callback is replayed.
export class Dispatcher {
  constructor(runtime) {
    this.runtime = runtime; this.sequence = 0; this.next = 0;
    this.lastDispatched = null; this.lastCompleted = null; this.callbacks = new Map();
  }
  async begin(store, kind, extension, budget) {
    // Interrupted callbacks are already detached and will never be replayed.
    // Bound their diagnostic retention without starving new healthy callbacks.
    if (this.callbacks.size >= 128) {
      const oldest = [...this.callbacks.values()].find(callback => callback.interrupted);
      if (oldest) this.callbacks.delete(oldest.id);
    }
    if (this.callbacks.size >= 128) invalid('dispatcher callback limit');
    const callback = { id: ++this.next, request_id: store.id, kind, extension,
      started: performance.now(), budget, store, interrupted: false };
    this.callbacks.set(callback.id, callback); this.lastDispatched = Date.now();
    await this.publish();
    return callback;
  }
  async finish(callback) {
    if (!callback) return;
    this.callbacks.delete(callback.id);
    this.lastCompleted = Date.now(); await this.publish();
  }
  async interrupt(callback) {
    if (!callback || callback.interrupted) return;
    callback.interrupted = true; await this.publish();
  }
  snapshot() {
    return { sequence: ++this.sequence, last_dispatched_at_ms: this.lastDispatched,
      last_completed_at_ms: this.lastCompleted,
      callbacks: [...this.callbacks.values()].map(callback => {
        const userWait = callback.store.userWait;
        const unbounded = callback.budget === undefined || userWait?.active > 0;
        return { id: callback.id, request_id: callback.request_id, kind: callback.kind,
          extension: callback.extension, interrupted: callback.interrupted,
          remaining_ms: unbounded || callback.interrupted ? null
            : Math.max(0, Math.ceil(callback.budget + (userWait?.spent() ?? 0) - (performance.now() - callback.started))) };
      }) };
  }
  publish() {
    if (!this.runtime.initialized || !this.runtime.features.has(LIVENESS_FEATURE) || this.runtime.stopping) return Promise.resolve();
    return this.runtime.transport.notify('status/contribution', { liveness: this.snapshot() });
  }
}

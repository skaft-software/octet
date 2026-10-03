// Native primitives remain private to transport/shutdown. Extension timers retain
// their AsyncLocalStorage owner and are revoked on surface/session settlement.
const native = {
  setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout,
  setInterval: globalThis.setInterval, clearInterval: globalThis.clearInterval,
};
export class Timers {
  constructor(runtime) { this.runtime = runtime; this.handles = new Map(); }
  install() {
    for (const kind of ['Timeout', 'Interval']) {
      globalThis[`set${kind}`] = (callback, delay, ...args) => {
        const store = this.runtime.scope.getStore();
        let handle;
        const run = () => {
          if (kind === 'Timeout') this.handles.delete(handle);
          if (store?.state && !store.state.alive || store?.surface?.closed) return;
          try {
            const result = this.runtime.scope.run(store, () => callback(...args));
            result?.catch?.(error => this.runtime.backgroundError(error));
          } catch (error) { this.runtime.backgroundError(error); }
        };
        handle = native[`set${kind}`](run, delay);
        this.handles.set(handle, { store, kind });
        return handle;
      };
      globalThis[`clear${kind}`] = handle => { this.handles.delete(handle); native[`clear${kind}`](handle); };
    }
  }
  clearWhere(match) {
    for (const [handle, value] of this.handles) if (match(value.store)) {
      native[`clear${value.kind}`](handle); this.handles.delete(handle);
    }
  }
  surface(surface) { this.clearWhere(s => s?.surface === surface); }
  owner(state) { this.clearWhere(s => s?.state === state); }
  all() { this.clearWhere(() => true); }
}
export const nativeDelay = milliseconds => new Promise(resolve => native.setTimeout(resolve, milliseconds));
export const deadline = (promise, milliseconds) => {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => { timer = native.setTimeout(() => reject(new Error('shutdown drain deadline')), milliseconds); })]).finally(() => native.clearTimeout(timer));
};

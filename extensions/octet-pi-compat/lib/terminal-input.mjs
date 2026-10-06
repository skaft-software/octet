import { bounded, fields, invalid, ownerKey, rpcError } from './errors.mjs';

// Pi 1.0.2 TuiBase.handleTerminalInput uses a live Set, not a snapshot or an
// awaited hook queue. Promises are not transformations; handlers are synchronous.
export class TerminalInputListeners {
  constructor(runtime) { this.runtime = runtime; this.listeners = new Map(); }
  add(handler, store) {
    this.runtime.require('terminal_input_intercept_v1');
    this.runtime.assertOwner(store);
    if (typeof handler !== 'function') invalid('terminal input handler');
    if (!this.listeners.has(handler)) {
      if (this.listeners.size >= 128) throw rpcError(-32012, 'bounds_exceeded terminal input listeners');
      this.listeners.set(handler, store);
    }
    return () => { this.listeners.delete(handler); };
  }
  retire(state) {
    for (const [handler, store] of this.listeners) if (store.state === state) this.listeners.delete(handler);
  }
  dispatch(params, request) {
    this.runtime.require('terminal_input_intercept_v1');
    fields(params, ['data', 'resource_owner'], 'terminal input');
    bounded(params.data, 'terminal input data', 256, { controls: true });
    const state = this.runtime.states.get(ownerKey(params.resource_owner));
    this.runtime.assertOwner({ state });
    let data = params.data;
    for (const [handler, captured] of this.listeners) {
      if (captured.state !== state) continue;
      request.controller.signal.throwIfAborted();
      this.runtime.assertFactory(captured.factory);
      // Retain the issued context, never confer a fresh effect-capable parent on
      // the listener. This request only decides how native input is dispatched.
      const result = this.runtime.scope.run(captured, () => handler(data));
      if (result?.then) { result.catch(error => this.runtime.backgroundError(error)); continue; }
      if (result?.consume) return { data: '' };
      if (result?.data !== undefined) {
        bounded(result.data, 'terminal input replacement', 256, { controls: true });
        data = result.data;
      }
    }
    return { data };
  }
}

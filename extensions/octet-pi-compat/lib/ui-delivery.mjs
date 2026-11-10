import { nativeTimer } from './timers.mjs';

// Rendering is a synchronous paint path: Pi components and tool renderers read
// UI state from local values and must never wait on the host while painting.
let renderDepth = 0;
export function rendering() { return renderDepth > 0; }
export function inRender(paint) {
  renderDepth++;
  try { return paint(); } finally { renderDepth--; }
}

// The host admits presentation under the real originating request. Give an
// answering frontend time to acknowledge it before that request settles and
// cancels its children, without letting a broken frontend stall a Pi hook.
export function joinPresentation(runtime, store, promise) {
  if (!store.live || runtime.active.get(store.id)?.controller !== store.controller || store.detached) return;
  const timer = nativeTimer(2000);
  runtime.track(Promise.race([Promise.resolve(promise).catch(() => {}), timer.promise])
    .finally(() => timer.cancel()), store);
}

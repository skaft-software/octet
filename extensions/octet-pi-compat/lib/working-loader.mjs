import { Loader } from '../node_modules/@earendil-works/pi-tui/dist/components/loader.js';
import { stripTerminalSequences } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';
import { fitLines } from './remote-ui.mjs';
import { theme } from './theme.mjs';
import { inRender } from './ui-delivery.mjs';

// Pi: packages/tui/src/components/loader.ts:37-116 owns animation, reset and
// verbatim indicator colors. Pi packages/coding-agent/src/modes/interactive/
// interactive-mode.ts:2447-2482,2739-2747,3486-3498,3714-3726 creates on
// work/show, retains settings, and disposes on agent_end.
// Only this instance is observed: dialog Loaders never publish working chrome.
export class WorkingLoader {
  constructor(runtime, state) {
    this.runtime = runtime; this.state = state;
    this.visible = true; this.generation = 0; this.pending = undefined;
    this.inFlight = false; this.scheduled = false; this.retired = false;
    this.store = { id: state.parent, state, controller: new AbortController(), live: false };
  }
  current() {
    return !this.retired && !this.runtime.stopping && this.state.alive
      && this.runtime.foreground === this.state && this.runtime.features.has('remote_ui');
  }
  safely(action) {
    if (!this.current()) return;
    try { this.runtime.scope.run(this.store, action); }
    catch { this.warn(); }
  }
  warn() {
    if (this.reported || !this.current()) return;
    this.reported = true;
    this.runtime.diagnostic('working Loader presentation failed; check or reload the Pi extensions');
  }
  message(value) {
    this.messageText = value;
    if (this.loader) this.safely(() => this.loader.setMessage(value ?? 'Working'));
  }
  indicator(value) {
    this.options = value;
    if (this.loader) this.safely(() => this.loader.setIndicator(value));
  }
  visibility(value) { this.visible = value; this.sync(); }
  sync() {
    if (!this.state.runActive && !this.loader) return;
    this.safely(() => {
      if (this.state.runActive && this.visible) {
        if (!this.loader) {
          const generation = ++this.generation;
          this.loader = new Loader({ requestRender: () => this.requestRender(generation) },
            text => theme.fg('accent', text), text => theme.fg('muted', text),
            this.messageText ?? 'Working', this.options);
        }
      } else if (this.loader) {
        this.loader.stop(); this.loader = undefined; ++this.generation;
        this.publish(null);
      }
    });
  }
  invalidate() { this.safely(() => this.loader?.invalidate()); }
  requestRender(generation) {
    if (!this.current() || generation !== this.generation) return;
    this.renderGeneration = generation;
    if (this.scheduled) return;
    this.scheduled = true;
    setImmediate(() => {
      this.scheduled = false;
      if (!this.current() || this.renderGeneration !== this.generation || !this.loader) return;
      this.safely(() => {
        // Capture AFTER updateDisplay (including prototype patches) returns.
        // Pi packages/coding-agent/src/modes/interactive/components/
        // status-indicator.ts:23-26 removes Loader's blank row/margins for
        // embedding. Rust owns final width/layout; this is a bounded single row.
        const lines = inRender(() => this.loader.render(4098));
        const line = lines.find(line => stripTerminalSequences(line).trim().length) ?? '';
        this.publish(fitLines([line.startsWith(' ') ? line.slice(1).trimEnd() : line.trimEnd()])[0]);
      });
    });
  }
  publish(line) {
    this.pending = { line, generation: this.generation };
    this.deliver();
  }
  deliver() {
    if (!this.current() || this.inFlight || this.pending === undefined) return;
    const snapshot = this.pending; this.pending = undefined;
    if (snapshot.generation !== this.generation) return;
    this.inFlight = true;
    // One request plus one replaceable latest snapshot, not an animation queue.
    // No live-parent/signal: local Pi chrome outlives its originating hook.
    Promise.resolve().then(() => {
      if (!this.current() || snapshot.generation !== this.generation) return;
      return this.runtime.transport.request('ui/chrome', {
        parent_request_id: this.state.parent, resource_owner: this.state.owner,
        chrome: { kind: 'working_component', line: snapshot.line },
      }, { timeout: 2000 });
    }).catch(() => this.warn()).finally(() => {
      this.inFlight = false;
      if (this.current()) this.deliver();
    });
  }
  retire() {
    this.retired = true; ++this.generation; this.pending = undefined;
    this.loader?.stop(); this.loader = undefined;
  }
}

const instances = new WeakMap();
export function workingLoader(runtime, state) {
  let instance = instances.get(state);
  if (!instance) instances.set(state, instance = new WorkingLoader(runtime, state));
  return instance;
}
export function retireWorkingLoader(state) { instances.get(state)?.retire(); }
export function invalidateWorkingLoader(state) { instances.get(state)?.invalidate(); }

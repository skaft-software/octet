// Real callbacks; tools provide deterministic barriers, not timers or fabricated ACKs.
const state = { mode: 'paths', results: undefined, calls: [], aborted: [], resumed: [], pending: new Map(), entered: new Map() };
function entered(label, event, ctx) {
  state.calls.push({ label, type: event.type, contextCwd: ctx.cwd,
    ...(event.type === 'resources_discover' ? { cwd: event.cwd, reason: event.reason } : {}) });
  state.entered.get(label)?.(); state.entered.delete(label);
}
async function hold(label, ctx) {
  const onAbort = () => state.aborted.push(label);
  ctx.signal.addEventListener('abort', onAbort, { once: true });
  try { await new Promise(resolve => state.pending.set(label, resolve)); }
  finally { ctx.signal.removeEventListener('abort', onAbort); }
  state.resumed.push(label);
}
const defaults = {
  first: { skillPaths: [' skills '], promptPaths: ['./prompts'], themePaths: ['./theme.toml'] },
  middle: { skillPaths: ['./more-skills'] },
  last: { skillPaths: ['./skills'] },
};
let removeMiddle;
export function registerResource(pi, label) {
  return pi.on('resources_discover', async (event, ctx) => {
    entered(label, event, ctx);
    const mode = state.mode, result = state.results ? state.results[label] : defaults[label];
    if (label === 'first') {
      if (mode === 'hold') await hold(label, ctx);
      if (mode === 'reverse') await pi.setSessionName('discovery reverse call');
      if (mode === 'throw') throw new Error('ordinary discovery callback failure');
      // An explicit octet refusal, not an absent member: absence now follows Pi.
      if (mode === 'unsupported') ctx.abort();
      if (mode === 'remove') removeMiddle();
      if (mode === 'mutate-event') { event.cwd = '/not-the-native-cwd'; event.reason = 'not-a-native-reason'; }
      if (mode === 'sparse') return { skillPaths: Array(1) };
      if (mode === 'array-extra') return { skillPaths: Object.assign(['./skills'], { unknown: true }) };
      if (mode === 'array-symbol') return { skillPaths: Object.assign(['./skills'], { [Symbol('extra')]: true }) };
      if (mode === 'symbol') return { [Symbol('extra')]: true };
      if (mode === 'hidden') return Object.defineProperty({}, 'extra', { value: true });
      if (mode === 'inherited') return Object.create({ skillPaths: ['./skills'] });
      if (mode === 'date') return new Date(0);
    }
    if (mode === 'empty') return label === 'first' ? {} : undefined;
    return result;
  });
}
export default pi => {
  pi.on('session_start', async (event, ctx) => { entered('session_start', event, ctx); if (state.mode === 'hold-start') await hold('session_start', ctx); });
  registerResource(pi, 'first');
  removeMiddle = registerResource(pi, 'middle');
  pi.registerTool({ name: 'resource_state', description: 'Observe/release discovery callback barriers', parameters: { type: 'object' },
    async execute(_id, args) {
      if (args.configure) {
        state.mode = args.configure.mode || 'paths'; state.results = args.configure.results;
        state.calls = []; state.aborted = []; state.resumed = [];
      }
      if (args.wait_for && !state.calls.some(call => call.label === args.wait_for)) await new Promise(resolve => state.entered.set(args.wait_for, resolve));
      if (args.release) { state.pending.get(args.release)?.(); state.pending.delete(args.release); }
      return { content: [{ type: 'text', text: JSON.stringify({ calls: state.calls, aborted: state.aborted, resumed: state.resumed, pending: [...state.pending.keys()] }) }] };
    },
  });
};

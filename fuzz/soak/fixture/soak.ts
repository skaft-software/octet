// Soak fixture: a minimal Pi extension exercising the bridge's provider
// pipeline, tool calls, and durable UI mounts.
//
// The bridge process environment is sanitized by the native host, so the
// provider URL travels through a file inside the Pi agent directory (which
// the host passes explicitly as OCTET_PI_AGENT_DIR).
//
// The provider delegates HTTP to the host (non-empty baseUrl), so every
// inference request reaches the deterministic loopback fake provider. The
// footer mount is intentionally opened from `session_start` without awaiting
// it, exactly like real Pi extensions do; on main the native reader cancels
// that mount when the hook replies (the known mount-cancellation race the
// reference-model fuzzer records). The soak harness reports mount liveness
// without failing on it until the fix lands.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

function providerUrl() {
  try {
    const agentDir = process.env.OCTET_PI_AGENT_DIR;
    if (!agentDir) throw new Error('no OCTET_PI_AGENT_DIR');
    const port = readFileSync(join(agentDir, 'soak-port'), 'utf8').trim();
    if (!/^\d+$/.test(port)) throw new Error('bad soak port');
    return `http://127.0.0.1:${port}/v1/`;
  } catch (error) {
    // The mirror capture imports this factory before the driver writes the
    // port file; a dummy loopback URL keeps the capture deterministic.
    return 'http://127.0.0.1:0/v1/';
  }
}

export default pi => {
  const baseUrl = providerUrl();
  let footerView = null;
  let turn = 0;
  let renders = 0;
  const footer = () => {
    turn += 1;
    renders += 1;
    footerView = { status: `soak turn ${turn}`, accent: 'green' };
    return footerView;
  };
  pi.on('session_start', (_event, ctx) => {
    // Pi's setFooter returns the component synchronously; the bridge returns
    // a promise, and awaiting it inside the hook would block session_start
    // forever on a headless host whose open acknowledgement never arrives
    // (the host cancels the open child when the hook replies on main). Real
    // Pi extensions never await this mount, so the fixture must not either.
    void ctx.ui.setFooter(() => footer());
  });
  pi.registerProvider('soak', {
    baseUrl,
    api: 'openai-responses',
    models: [{
      id: 'soak-model',
      name: 'Soak Model',
      api: 'openai-responses',
      baseUrl,
      reasoning: false,
      input: ['text'],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 32768,
      maxTokens: 1024,
    }],
    // Required for explicit models; the delegated host route performs the
    // HTTP, so this generator must never be invoked.
    streamSimple: async function* (_input, _update, _signal) {
      throw new Error('soak provider delegates HTTP to the host');
    },
  });
  pi.registerTool({
    name: 'soak_probe',
    description: 'Soak probe: returns the requested turn number.',
    parameters: {
      type: 'object',
      properties: { turn: { type: 'number' } },
      required: ['turn'],
    },
    async execute(_id, args, _signal, _update, _ctx) {
      return { content: [{ type: 'text', text: `probe turn ${args.turn}` }] };
    },
  });
  pi.registerTool({
    name: 'soak_ui',
    description: 'Soak UI: updates the footer and returns the last frame.',
    parameters: { type: 'object', properties: {} },
    async execute(_id, _args, _signal, _update, ctx) {
      const view = ctx.ui.setFooter(() => footer());
      return { content: [{ type: 'text', text: JSON.stringify({ view: view ?? null, renders, turn }) }] };
    },
  });
};

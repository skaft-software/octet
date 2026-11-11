// The provider URL travels through a file the driver writes next to this
// fixture (<workspace>/.pi/soak-port). It is NOT read from the environment:
// the host sandboxes the bridge's environment, and the factory runs in more
// than one bridge process (the activation-time mirror capture and the runtime
// bridge), so an environment lookup would silently fall back to a dummy URL in
// one of them and the provider would be registered with a dead endpoint.
// The fixture only reads, and only inside a directory the soak driver marked
// with a `.soak-harness` sentinel; it never writes.
import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { streamSimpleOpenAIResponses } from '@earendil-works/pi-ai';

function harnessDir() {
  const here = dirname(fileURLToPath(import.meta.url)); // <workspace>/.pi/extensions
  const dir = join(here, '..');
  if (!existsSync(join(dir, '.soak-harness'))) throw new Error('not a soak harness directory');
  return dir;
}

function providerUrl() {
  const port = readFileSync(join(harnessDir(), 'soak-port'), 'utf8').trim();
  if (!/^\d+$/.test(port)) throw new Error('bad soak port');
  return `http://127.0.0.1:${port}/v1/`;
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
    // Pi requires an apiKey (or oauth) whenever models are defined.
    apiKey: 'soak-key',
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
    // Pi runs the extension's own stream for its provider. Delegating to the
    // installed pi-ai's OpenAI Responses stream sends the request to baseUrl
    // (the loopback fake) exactly as a real Pi custom provider would.
    streamSimple: (model, context, options) => streamSimpleOpenAIResponses(model, context, options),
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

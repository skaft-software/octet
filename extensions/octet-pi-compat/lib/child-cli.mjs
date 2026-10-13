// Observed print/JSON CLI subset. Unknown argv is a refusal, never a silently
// ignored Pi session/resource option or a fallback to an upstream Pi agent.
import { createAgentSession, withChildHost, readTool, bashTool, editTool, writeTool } from './children.mjs';
import { bounded, invalid, unsupported } from './errors.mjs';
const TOOLS = { read: readTool, bash: bashTool, edit: editTool, write: writeTool };
export function parseChildArgv(argv) {
  if (!Array.isArray(argv) || argv.length > 128) invalid('child argv');
  let mode, print = false, prompt, model, provider, thinkingLevel, tools;
  for (let i = 0; i < argv.length; i++) {
    const arg = bounded(argv[i], 'child argv', 131072, { controls: true });
    const value = () => { if (i + 1 >= argv.length) invalid(`missing value for ${arg}`); return bounded(argv[++i], arg, 1024); };
    if (arg === '--mode') mode = value();
    else if (arg === '-p' || arg === '--print') print = true;
    else if (arg === '--no-session') { /* No Pi-format file is created; native persistence remains mandatory. */ }
    else if (arg === '--model') model = value();
    else if (arg === '--provider') provider = value();
    else if (arg === '--thinking') thinkingLevel = value();
    else if (arg === '--tools') {
      const names = value().split(',');
      if (!names.length || names.some(name => !TOOLS[name])) unsupported('--tools', 'only exact native read, bash, edit, write descriptors are supported');
      tools = names.map(name => TOOLS[name]);
    } else if (arg === '--') {
      if (i + 2 !== argv.length || prompt !== undefined) invalid('exactly one child prompt is required');
      prompt = bounded(argv[++i], 'child prompt', 131072, { controls: true });
    } else if (arg.startsWith('-')) unsupported(`Pi CLI ${arg}`, 'session files, resource loading, system-prompt overrides and nested callbacks are not yet bound');
    else if (prompt !== undefined) invalid('multiple positional child prompts');
    else prompt = arg;
  }
  if (mode !== 'json' || !print) unsupported('Pi CLI mode', 'only --mode json -p is supported');
  if (!prompt?.trim()) invalid('one explicit child prompt is required; no implicit stdin/interactive agent');
  if (prompt.startsWith('@')) unsupported('Pi CLI file prompt', 'file expansion must be admitted through the host, not read by this facade');
  const options = { ...(tools ? { tools } : {}), ...(thinkingLevel ? { thinkingLevel } : {}) };
  if (model) {
    const slash = model.indexOf('/');
    options.model = slash >= 0 ? { provider: model.slice(0, slash), id: model.slice(slash + 1) } : { provider: provider ?? 'inherit', id: model };
    if (provider && options.model.provider !== provider) invalid('conflicting child model/provider');
  } else if (provider) unsupported('--provider without --model', 'host provider selection requires an explicit model');
  return { prompt, options };
}

export async function runChildCli(argv, { binding, emit, signal } = {}) {
  if (typeof emit !== 'function') invalid('JSON event writer required');
  const { prompt, options } = parseChildArgv(argv);
  signal?.throwIfAborted();
  return withChildHost(binding, async () => {
    const { session } = await createAgentSession(options);
    const unsubscribe = session.subscribe(emit);
    let abort;
    const cancelled = () => { abort ??= session.abort(); abort.catch(() => {}); };
    signal?.addEventListener('abort', cancelled, { once: true });
    try {
      signal?.throwIfAborted();
      await session.prompt(prompt);
      if (abort) await abort;
      return signal?.aborted ? 130 : 0;
    } finally {
      signal?.removeEventListener('abort', cancelled); unsubscribe();
      await session.dispose();
    }
  });
}

import { Type } from '@mariozechner/pi-ai';
import { defineTool } from '@earendil-works/pi-coding-agent';
import { isKeyRelease, isKeyRepeat, parseKey, Text } from '@mariozechner/pi-tui';
export default function(pi) {
  console.log('factory console diagnostic');
  process.stdout.write('factory direct stdout diagnostic\n');
  pi.registerFlag('test-option', { type: 'string', default: 'default', description: 'Test flag' });
  pi.registerCommand('surface', { description: 'Mount a retained component', async handler(args, ctx) {
    const value = await ctx.ui.custom((tui, theme, _keys, done) => {
      let ticks = 0, last = '', invalidated = 0;
      const timer = setInterval(() => { ticks++; tui.requestRender(); }, 20);
      return {
        wantsKeyRelease: true,
        render(width) { return [theme.fg('accent', `${ctx.model?.name}|${ctx.sessionManager.getSessionName()}|${width}x${tui.terminal.rows}|tick=${ticks}|invalidated=${invalidated}|${last}`)]; },
        invalidate() { invalidated++; },
        handleInput(data) {
          last = `${parseKey(data)}:${isKeyRelease(data) ? 'release' : isKeyRepeat(data) ? 'repeat' : 'press'}`;
          if (data === 'q') done('completed');
          tui.requestRender();
        },
        dispose() { clearInterval(timer); ctx.ui.notify('disposed'); },
      };
    });
    ctx.ui.notify(`done:${value}`);
  } });
  pi.registerCommand('credentials', { description: 'Resolve scoped model credentials through the host', async handler(args, ctx) {
    const model = args ? { ...ctx.model, provider: args.split(':')[0], id: args.split(':')[1] } : ctx.model;
    const result = await ctx.modelRegistry.getApiKeyAndHeaders(model);
    return { content: [{ type: 'text', text: JSON.stringify(result) }] };
  } });
  pi.registerCommand('select', { description: 'Real selection', async handler(_, ctx) { ctx.ui.notify(`selected:${await ctx.ui.select('Choose', ['one', 'two'])}`); } });
  pi.registerCommand('chrome', { description: 'Mount chrome', handler(_, ctx) {
    ctx.ui.setHeader(() => new Text('HEADER', 0, 0));
    ctx.ui.setFooter((tui, _theme, data) => ({ render: () => [`FOOTER ${ctx.model?.id} ${data.getExtensionStatuses().size}`], invalidate() {} }));
    ctx.ui.setWidget('a', ['ABOVE']);
    ctx.ui.setWidget('b', ['BELOW'], { placement: 'belowEditor' });
  } });
  pi.registerCommand('clear', { description: 'Clear chrome', handler(_, ctx) { ctx.ui.setHeader(undefined); ctx.ui.setFooter(undefined); ctx.ui.setWidget('a', undefined); ctx.ui.setWidget('b', undefined); } });
  pi.registerTool(defineTool({ name: 'core', label: 'Core', description: 'Exercise bridge operations', parameters: Type.Object({ mode: Type.String() }),
    async execute(_id, { mode }, signal, update, ctx) {
      if (mode === 'wait') {
        update?.({ content: [{ type: 'text', text: 'waiting' }] });
        await new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }));
      }
      if (mode === 'confirm') return { content: [{ type: 'text', text: String(await ctx.ui.confirm('Continue?', 'Synthetic host')) }] };
      if (mode === 'credentials' || mode.startsWith('credentials:')) {
        const [, provider, id] = mode.split(':');
        const model = provider ? { ...ctx.model, provider, id } : ctx.model;
        const result = await ctx.modelRegistry.getApiKeyAndHeaders(model);
        return { content: [{ type: 'text', text: JSON.stringify(result) }] };
      }
      if (mode === 'mutate') {
        ctx.ui.setEditorText('local'); pi.setSessionName('renamed');
        return { content: [{ type: 'text', text: `${ctx.ui.getEditorText()}|${ctx.sessionManager.getSessionName()}` }] };
      }
      if (mode === 'unsupported') ctx.ui.setTitle('cannot be honored');
      // Print-mode behavior: presentation setters are inert headless, while a
      // dialog and the native theme catalog still need a real frontend.
      if (mode === 'chrome') ctx.ui.setTitle('inert headless');
      if (mode === 'dialog') ctx.ui.custom(() => ({ render: () => [], invalidate() {} }));
      if (mode === 'theme') ctx.ui.getAllThemes();
      if (mode === 'unsafe-output') process.stdout.write('\x1b[2J');
      if (mode === 'runtime') { const { createAgentSession } = await import('@mariozechner/pi-coding-agent'); await createAgentSession(); }
      if (mode === 'media') return { content: [{ type: 'image', data: 'xx', mimeType: 'image/png' }] };
      if (mode === 'fail') throw new Error('handler crash');
      return { content: [{ type: 'text', text: `${ctx.cwd}|${ctx.hasUI}|${pi.getFlag('test-option')}` }], details: { model: ctx.model } };
    },
  }));
  pi.on('session_start', (_, ctx) => { ctx.ui.setStatus('fixture', 'active'); });
  pi.on('tool_call', async event => {
    if (event.toolName === 'ordered') { await new Promise(r => setTimeout(r, event.input.delay || 0)); console.log(`ordered:${event.input.order}`); }
    return event.toolName === 'blocked' ? { block: true, reason: 'test veto' } : undefined;
  });
}

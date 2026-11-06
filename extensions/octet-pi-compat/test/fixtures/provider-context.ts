export default function (pi) {
  pi.on('context', async (event, ctx) => {
    const mode = ctx.sessionManager.getSessionName();
    if (mode === 'wait') await ctx.ui.confirm('context barrier');
    if (mode === 'append') {
      const before = ctx.sessionManager.getLeafId();
      const result = pi.appendEntry('context-proof', { before, text: 'durable\ncheckpoint' });
      if (result !== undefined) throw new Error('appendEntry must be synchronous void');
      event.messages.push({ role: 'custom', customType: 'receipt', display: false,
        content: `known:${ctx.sessionManager.getLeafId()}:${ctx.sessionManager.getBranch().at(-1).id}` });
    }
    if (mode === 'replace') return { messages: [{ role: 'user', content: 'projected' }] };
    if (mode === 'throws-coded') throw Object.assign(new Error('coded context failure'), { code: -32602 });
    if (mode === 'in-place') { event.messages.splice(0, 1); event.messages.push({ role: 'user', content: 'mutated' }); }
    if (mode === 'unknown-field') return { messages: [], tools: [] };
    if (mode === 'unknown-role') return { messages: [{ role: 'system', content: 'cannot create a hidden provider role' }] };
    if (mode === 'malformed') return { messages: [null] };
    if (mode === 'throws') throw new Error('ordinary context failure');
  });
  pi.on('context', (event, ctx) => {
    const mode = ctx.sessionManager.getSessionName();
    if (['replace', 'in-place', 'throws', 'throws-coded', 'wait'].includes(mode)) {
      event.messages.push({ role: 'user', content: `second:${ctx.getSystemPrompt()}` });
    }
  });
  pi.registerCommand('idle-proof', { handler: async (_args, ctx) => {
    await ctx.waitForIdle(); ctx.ui.notify('real idle receipt');
  } });
}

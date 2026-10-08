export default pi => {
  pi.on('before_agent_start', async (event, ctx) => {
    if (event.type !== 'before_agent_start' || event.images !== undefined) throw new Error('bad actual event');
    if (ctx.sessionManager.getSessionName() === 'wait') await ctx.ui.confirm('wait', 'before replacement');
    if (event.prompt === 'unsupported') return { message: { customType: 'not-bound', content: 'not dropped' } };
    if (event.prompt === 'nul') return { systemPrompt: 'bad\0system' };
    if (event.prompt === 'unchanged') return;
    if (event.prompt === 'empty') return { systemPrompt: '' };
    return { systemPrompt: `${event.systemPrompt}\nfirst` };
  });
  pi.on('before_agent_start', event => event.prompt === 'chain' ? { systemPrompt: `${event.systemPrompt}\nsecond` } : undefined);
}

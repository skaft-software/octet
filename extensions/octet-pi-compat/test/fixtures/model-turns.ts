export default pi => {
  pi.on('turn_start', async (event, ctx) => {
    const mode = ctx.sessionManager.getSessionName();
    if (mode === 'wait') await ctx.ui.confirm('model-start barrier', 'wait');
    if (mode === 'veto') return { cancel: true };
    ctx.ui.notify(`model-start:${event.turnIndex}:${event.timestamp}`);
  });
  pi.on('turn_end', (event, ctx) => {
    const mode = ctx.sessionManager.getSessionName();
    if (mode === 'usage') void event.message.usage;
    if (mode === 'append') pi.appendEntry('turn-observed', { index: event.turnIndex, text: event.message.content[0].text });
    ctx.ui.notify('model-end:' + JSON.stringify({ index: event.turnIndex, message: event.message, tools: event.toolResults }));
  });
  pi.on('agent_start', (_event, ctx) => { ctx.ui.notify('whole-run-start'); });
  pi.on('agent_end', (_event, ctx) => { ctx.ui.notify('whole-run-end'); });
}

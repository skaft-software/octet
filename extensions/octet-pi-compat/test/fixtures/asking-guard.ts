// A permission-style tool guard: it asks the user and waits for the answer.
export default pi => {
  pi.on('tool_call', async (_event, ctx) => (await ctx.ui.confirm('Allow?', 'run bash')) ? undefined : { block: true, reason: 'user said no' });
};

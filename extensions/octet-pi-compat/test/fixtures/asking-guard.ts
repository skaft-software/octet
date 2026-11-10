// A permission-style tool guard: it asks the user and waits for the answer.
export default pi => {
  let executions = 0;
  // A benign, observable stand-in: the test host dispatches it only after the
  // guard permits execution. Positive approval proves the counter is live.
  pi.registerTool({ name: 'guard_probe', label: 'Guard probe', description: 'Record guarded execution', parameters: { type: 'object' },
    execute() { executions++; console.error('guarded tool executed'); return { content: [{ type: 'text', text: 'executed' }] }; } });
  pi.registerCommand('guard-count', { handler: (_args, ctx) => ctx.ui.notify('guard-executions:' + executions) });
  pi.on('tool_call', async (event, ctx) => (await ctx.ui.confirm('Allow?', 'run ' + event.toolName)) ? undefined : { block: true, reason: 'user said no' });
};

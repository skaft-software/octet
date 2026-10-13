// What an extension footer reads from the host: context usage and whether the
// active model's provider uses a subscription login.
export default pi => {
  pi.on('session_start', () => {});
  pi.registerTool({
    name: 'facts', label: 'Facts', description: 'Report what the host says about the model',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    async execute(_id, _args, _signal, _update, ctx) {
      return { content: [{ type: 'text', text: JSON.stringify({ usage: ctx.getContextUsage() ?? null, oauth: ctx.model ? (() => { try { return ctx.modelRegistry.isUsingOAuth(ctx.model); } catch (error) { return 'refused'; } })() : null }) }] };
    },
  });
};

export default pi => {
  for (const event of ['agent_start', 'turn_start', 'turn_end']) {
    pi.on(event, (_event, ctx) => {
      ctx.ui.notify(`${event}-prompt:${ctx.getSystemPrompt()}`);
    });
  }
};

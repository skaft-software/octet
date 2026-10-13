export default pi => {
  pi.on('session_start', () => {});
  pi.on('agent_end', (_event, ctx) => { ctx.ui.notify('run:end'); });
  pi.on('agent_settled', (event, ctx) => { ctx.ui.notify('run:settled:' + JSON.stringify(event)); });
}

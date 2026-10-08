export default pi => {
  pi.on('agent_end', (event, ctx) => { ctx.ui.notify('standalone:' + JSON.stringify(event)); });
};

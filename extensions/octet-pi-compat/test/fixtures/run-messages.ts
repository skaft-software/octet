export default function (pi) {
  pi.on('before_agent_start', () => undefined);
  for (const type of ['agent_start', 'agent_end', 'message_start', 'message_update', 'message_end']) {
    pi.on(type, (event, ctx) => ctx.ui.notify('evt:' + JSON.stringify(event)));
  }
}

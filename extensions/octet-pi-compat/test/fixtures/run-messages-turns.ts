import messages from './run-messages.ts';
export default function (pi) {
  messages(pi);
  for (const type of ['turn_start', 'turn_end']) {
    pi.on(type, (event, ctx) => ctx.ui.notify('evt:' + JSON.stringify(event)));
  }
}

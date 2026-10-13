// Decode only one complete, safe OSC777 notification. Extensions never acquire
// the protocol writer or a terminal; the native owner-fenced frontend emits it.
import { bounded, fields, invalid } from './errors.mjs';
export function notificationIntent(text) {
  const match = /^\x1b\]777;notify;([^;\x00-\x1f\x7f-\x9f]*);([^\x00-\x1f\x7f-\x9f]*)\x07$/.exec(text);
  if (!match) return undefined;
  return { kind: 'desktop_notification', title: bounded(match[1], 'notification title', 1024), body: bounded(match[2], 'notification body', 4096) };
}
export function sendDesktopNotification(runtime, notification) {
  runtime.require('remote_ui');
  const store = runtime.scope.getStore(); runtime.assertOwner(store);
  store.controller.signal.throwIfAborted();
  const receipt = runtime.transport.requestSync('ui/chrome', {
    parent_request_id: store.id, resource_owner: store.state.owner, chrome: notification,
  }, { parent: store.live ? store.id : undefined, signal: store.controller.signal });
  fields(receipt, ['tools_expanded'], 'notification receipt');
  if (typeof receipt.tools_expanded !== 'boolean') invalid('notification receipt');
}

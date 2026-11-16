// Pi packages/coding-agent/src/core/extensions/runner.ts:1095-1128 (veto),
// :730-750 (stale contexts), types.ts:429-450 (new/setup/withSession/switch/fork).
// SessionManager is canonical; observations below test IDs/ancestry without comparing generated IDs.
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
const trace = value => appendFileSync('conformance-trace.jsonl', JSON.stringify(value) + '\n');
export default pi => {
  let originalFile, originalId, cancel = false;
  pi.on('session_before_switch', () => cancel ? { cancel: true } : undefined);
  pi.registerCommand('conform-state', { handler: (_, ctx) => {
    originalFile = ctx.sessionManager.getSessionFile();
    originalId = ctx.sessionManager.getSessionId();
    writeFileSync('conformance-session.json', JSON.stringify({ originalFile, originalId }));
    pi.setSessionName('Conformance session');
    pi.appendEntry('conformance-private', { value: 'PRIVATE_SENTINEL' });
    const entry = ctx.sessionManager.getBranch().at(-1);
    trace({ stage: 'state', named: pi.getSessionName() === 'Conformance session',
      appended: entry.type === 'custom' && entry.customType === 'conformance-private',
      leaf: ctx.sessionManager.getLeafId() === entry.id });
    ctx.ui.notify('CONFORMANCE_STATE');
  } });
  pi.registerCommand('conform-cancel', { handler: async (_, ctx) => {
    cancel = true;
    const result = await ctx.newSession();
    cancel = false;
    trace({ stage: 'cancel', cancelled: result.cancelled, same: ctx.sessionManager.getSessionId() === originalId });
    ctx.ui.notify('CONFORMANCE_CANCELLED');
  } });
  pi.registerCommand('conform-new', { handler: async (_, ctx) => {
    await ctx.newSession({ setup: async sm => { sm.appendCustomEntry('conformance-setup', { ready: true }); },
      withSession: async fresh => {
        trace({ stage: 'new', changed: fresh.sessionManager.getSessionId() !== originalId,
          setup: fresh.sessionManager.getEntries().some(e => e.type === 'custom' && e.customType === 'conformance-setup') });
        fresh.ui.notify('CONFORMANCE_NEW');
      } });
  } });
  pi.registerCommand('conform-return', { handler: async (_, ctx) => {
    ({ originalFile, originalId } = JSON.parse(readFileSync('conformance-session.json', 'utf8')));
    await ctx.switchSession(originalFile, { withSession: async fresh => {
      trace({ stage: 'return', same: fresh.sessionManager.getSessionId() === originalId,
        restored: fresh.sessionManager.getEntries().some(e => e.type === 'custom' && e.customType === 'conformance-private') });
      fresh.ui.notify('CONFORMANCE_RETURN');
    } });
  } });
};

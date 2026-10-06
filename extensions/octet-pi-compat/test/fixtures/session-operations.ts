export default pi => {
  pi.on('session_before_compact', (event, ctx) => {
    if (event.reason !== 'manual') return { cancel: true };
    if (ctx.sessionManager.getSessionName() === 'extra') return { compaction: { summary: 'summary', firstKeptEntryId: 'kept', tokensBefore: 1 } };
    return { compaction: { summary: 'real replacement', firstKeptEntryId: event.preparation.firstKeptEntryId } };
  });
  pi.on('session_compact', (event, ctx) => { ctx.ui.notify('committed:' + event.compactionEntry.id); });
  pi.on('session_before_tree', () => ({ cancel: true }));
  pi.on('session_tree', event => { pi.appendEntry('tree-proof', { old: event.oldLeafId, current: event.newLeafId }); });
}

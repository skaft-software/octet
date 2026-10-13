export default pi => {
  pi.on('session_before_compact', (event, ctx) => {
    if (event.reason !== 'manual') return { cancel: true };
    if (ctx.sessionManager.getSessionName() === 'extra') return { compaction: { summary: 'summary', firstKeptEntryId: 'kept', tokensBefore: 1 } };
    return { compaction: { summary: 'real replacement', firstKeptEntryId: event.preparation.firstKeptEntryId } };
  });
  pi.on('session_compact', (event, ctx) => { ctx.ui.notify('committed:' + event.compactionEntry.id); });
  pi.on('session_before_tree', (event, ctx) => {
    if (ctx.sessionManager.getSessionName() === 'summary') ctx.ui.notify('tree-preparation:' + JSON.stringify({ targetId: event.preparation.targetId,
      oldLeafId: event.preparation.oldLeafId, commonAncestorId: event.preparation.commonAncestorId,
      entriesToSummarize: event.preparation.entriesToSummarize, userWantsSummary: event.preparation.userWantsSummary,
      customInstructions: event.preparation.customInstructions }));
    return { cancel: true };
  });
  pi.on('session_tree', event => { pi.appendEntry('tree-proof', { old: event.oldLeafId, current: event.newLeafId,
    ...(Object.hasOwn(event, 'summaryEntry') ? { summary: event.summaryEntry, fromExtension: event.fromExtension } : {}) }); });
}

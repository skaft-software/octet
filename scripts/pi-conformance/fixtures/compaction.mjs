// Pi packages/coding-agent/src/core/agent-session.ts:2849-2961:
// awaited before hook precedes summary generation; compact event follows durable append.
// :3535-3546 ctx.compact is void and delivers completion asynchronously.
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync('conformance-trace.jsonl', JSON.stringify(value) + '\n');
export default pi => {
  pi.on('session_before_compact', () => { trace({ stage: 'before' }); });
  pi.on('session_compact', (e, ctx) => trace({ stage: 'compacted', summary: e.compactionEntry.summary,
    durable: ctx.sessionManager.getEntries().some(entry => entry.id === e.compactionEntry.id) }));
  pi.registerCommand('conform-compact', { handler: (_, ctx) => {
    const result = ctx.compact({ onComplete: result => {
      trace({ stage: 'complete', summary: result.summary });
      ctx.ui.notify('CONFORMANCE_COMPACTED');
    }, onError: () => { trace({ stage: 'error' }); ctx.ui.notify('CONFORMANCE_COMPACT_ERROR'); } });
    trace({ stage: 'return', void: result === undefined });
  } });
};

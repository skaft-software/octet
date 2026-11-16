// Pi packages/coding-agent/src/core/agent-session.ts:2542-2581 and :3509-3513:
// setModel returns true after selection and emits model_select with previous model.
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync('conformance-trace.jsonl', JSON.stringify(value) + '\n');
export default pi => {
  pi.on('model_select', e => trace({ stage: 'select', id: e.model.id, previous: e.previousModel?.id, source: e.source }));
  pi.registerCommand('conform-model', { handler: async (_, ctx) => {
    const model = ctx.modelRegistry.find(ctx.model.provider, 'probe-next');
    const selected = await pi.setModel(model);
    trace({ stage: 'selected', selected, id: ctx.model.id });
    ctx.ui.notify('CONFORMANCE_MODEL_SELECTED');
  } });
};

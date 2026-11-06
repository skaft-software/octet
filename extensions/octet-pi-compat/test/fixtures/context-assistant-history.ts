// A context hook that behaves like pi-goal's: it asks the session history
// (branch entries and the built context messages) whether any item is a goal
// contract. Its unwrap step reads `.type` and `.customType` on every item, so it
// reads keys that a Pi assistant message does not have. In Pi those read as
// undefined; the hook must not fail because the history holds an assistant turn.
export default function (pi) {
  const unwrap = (item) => (item?.type === 'custom_message' ? item : item?.message ?? item ?? {});
  const marked = (items) => items.some((item) => unwrap(item).customType === 'goal-contract');
  pi.on('context', (event, ctx) => {
    const manager = ctx.sessionManager;
    const kept = event.messages.filter((message) => message.role !== 'custom' || message.customType !== 'budget');
    const found = marked(kept) || marked(manager.buildSessionContext().messages) || marked(manager.getBranch())
      || marked(manager.getEntries());
    if (found) return { messages: [...kept, { role: 'custom', customType: 'goal-contract', content: 'found', display: false, timestamp: 0 }] };
  });
}

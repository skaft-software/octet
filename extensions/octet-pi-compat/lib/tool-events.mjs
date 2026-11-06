export function toolLifecyclePayload(payload, store, method = payload.result === undefined ? 'tool/started' : 'tool/settled') {
  const toolCallId = payload.tool_call_id;
  const toolName = payload.tool_name;
  const calls = store.state.toolCalls ??= new Map();
  if (payload.args !== undefined) calls.set(toolCallId, structuredClone(payload.args));
  const args = calls.get(toolCallId) ?? {};
  const event = { toolCallId, toolName, ...(method === 'tool/settled' ? {} : { args }) };
  if (method === 'tool/settled' && Number.isSafeInteger(payload.duration_ms)) event.durationMs = payload.duration_ms;
  if (payload.partialResult !== undefined) event.partialResult = payload.partialResult;
  if (payload.result !== undefined) {
    event.result = payload.result;
    event.isError = payload.isError === true;
  }
  if (method === 'tool/settled') {
    calls.delete(toolCallId);
  }
  return event;
}

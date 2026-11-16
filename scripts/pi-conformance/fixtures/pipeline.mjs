// Pi source (owner reference): packages/coding-agent/src/core/extensions/runner.ts:
// 1525-1574 input chaining, 1305-1335 context, 1368-1426 payload/header hooks,
// 1190-1246 tool result chaining. agent-session.ts:3487-3493 appendEntry.
import { appendFileSync } from 'node:fs';
const trace = value => appendFileSync('conformance-trace.jsonl', JSON.stringify(value) + '\n');
export default pi => {
  pi.on('input', e => ({ action: 'transform', text: e.text + ':INPUT1' }));
  pi.on('input', e => { trace({ stage: 'input', text: e.text }); return { action: 'transform', text: e.text + ':INPUT2' }; });
  pi.on('before_agent_start', e => { trace({ stage: 'prompt', text: e.prompt }); return { systemPrompt: 'CONFORMANCE_SYSTEM' }; });
  pi.on('context', e => {
    const user = e.messages.findLast(m => m.role === 'user');
    if (user) user.content = [{ type: 'text', text: 'CONFORMANCE_CONTEXT' }];
  });
  pi.on('before_provider_request', e => ({ ...e.payload, max_tokens: 77 }));
  pi.on('before_provider_request', e => ({ ...e.payload, max_tokens: e.payload.max_tokens + 1 }));
  pi.on('before_provider_headers', e => { e.headers['x-conformance'] = 'first'; });
  pi.on('before_provider_headers', e => { e.headers['x-conformance'] += ':second'; });
  pi.on('after_provider_response', e => trace({ stage: 'response', status: e.status }));
  pi.on('tool_call', e => { if (e.toolName === 'conformance_echo') e.input.value += ':CALL'; });
  pi.on('tool_result', e => ({ content: [{ type: 'text', text: e.content[0].text + ':RESULT' }] }));
  pi.registerTool({ name: 'conformance_echo', label: 'Conformance echo', description: 'Side-effect-free conformance echo',
    parameters: { type: 'object', properties: { value: { type: 'string' } }, required: ['value'], additionalProperties: false },
    async execute(_id, args) {
      trace({ stage: 'execute', value: args.value });
      return { content: [{ type: 'text', text: 'ECHO:' + args.value }] };
    },
  });
};

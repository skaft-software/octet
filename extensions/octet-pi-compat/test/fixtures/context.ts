import { calculateContextTokens, estimateTokens, buildSessionContext } from '@earendil-works/pi-coding-agent';
import * as legacy from '@mariozechner/pi-coding-agent';

export default function (pi) {
  if (legacy.estimateTokens !== estimateTokens || legacy.buildSessionContext !== buildSessionContext
      || legacy.calculateContextTokens !== calculateContextTokens) throw new Error('helper aliases diverged');
  // Exercise actual values at factory time, not just successful named imports.
  const sample = buildSessionContext([{ type: 'message', id: 'u', parentId: null,
    message: { role: 'user', content: 'hello' } }]);
  pi.registerTool({
    name: 'context_helpers', label: 'Pure context helpers',
    description: `Pure helper import estimate: ${estimateTokens(sample.messages[0])}`,
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    async execute() {
      return { content: [{ type: 'text', text: JSON.stringify({
        tokens: calculateContextTokens({ input: 5, output: 2, cacheRead: 3, cacheWrite: 4, totalTokens: 0 }),
        estimate: estimateTokens({ role: 'user', content: [{ type: 'image', data: '', mimeType: 'image/png' }] }),
        context: sample,
      }) }] };
    },
  });
}

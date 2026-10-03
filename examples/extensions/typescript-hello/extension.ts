import { Extension } from '@skaft-software/octet-extension-sdk';

const extension = new Extension({maxConcurrentRequests: 2});
extension.tool({
  name: 'text_stats',
  description: 'Count characters, words, lines and UTF-8 bytes in supplied text locally',
  parameters: {
    type: 'object',
    properties: {
      text: {type: 'string', maxLength: 65_536},
      delayMs: {type: 'integer', minimum: 0, maximum: 2000, description: 'Optional cancellable delay for local testing'},
    },
    required: ['text'],
    additionalProperties: false,
  },
}, async ({text, delayMs = 0}, context) => {
  context.throwIfCancelled();
  if (context.supportsProgress) await context.progress('Counting supplied text');
  await context.sleep(delayMs);
  const words = text.trim() ? text.trim().split(/\s+/u).length : 0;
  const lines = text ? text.split(/\r\n|\r|\n/u).length : 0;
  return `characters=${[...text].length} words=${words} lines=${lines} utf8_bytes=${new TextEncoder().encode(text).length}`;
});
export default extension;

// Ordinary Pi hooks only; the existing core.ts factory is loaded unmodified.
import { appendFileSync } from 'node:fs';

export default function (pi) {
  const record = (kind, fields = {}) => appendFileSync('pi-calls.jsonl',
    JSON.stringify({ pid: process.pid, kind, ...fields }) + '\n');
  record('factory');
  pi.on('tool_result', event => {
    if (event.toolName === 'core') {
      record('tool_result', { tool: event.toolName, content: event.content, is_error: event.isError });
    }
  });
}

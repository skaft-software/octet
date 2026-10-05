// Pure Pi 1.0.2 extension-authoring helpers, adapted from the MIT-licensed
// coding-agent sources at 200387122ca450d6387f033949423114a270b96c.
// See ../LICENSE.pi. No Pi agent, session store, or CLI runtime is loaded.
import { realpath } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { homedir } from 'node:os';
import { fileURLToPath } from 'node:url';

export const CONFIG_DIR_NAME = '.pi';
export function getAgentDir() {
  let path = process.env.PI_CODING_AGENT_DIR;
  if (!path) return join(homedir(), CONFIG_DIR_NAME, 'agent');
  if (process.platform === 'win32' && path.startsWith('/') && !path.startsWith('//') && !path.includes('\\')) {
    const match = path.match(/^\/(?:mnt\/|cygdrive\/)?([a-z])(?:\/(.*))?$/i);
    if (match) path = `${match[1].toUpperCase()}:\\${match[2]?.replaceAll('/', '\\') ?? ''}`;
  }
  if (path === '~') return homedir();
  if (path.startsWith('~/') || (process.platform === 'win32' && path.startsWith('~\\'))) return join(homedir(), path.slice(2));
  return path.startsWith('file://') ? fileURLToPath(path) : path;
}
import { parse } from 'yaml';

export const isToolCallEventType = (toolName, event) => event.toolName === toolName;
export const isBashToolResult = event => event.toolName === 'bash';
export const isPowerShellToolResult = event => event.toolName === 'powershell';
export const isReadToolResult = event => event.toolName === 'read';
export const isEditToolResult = event => event.toolName === 'edit';
export const isWriteToolResult = event => event.toolName === 'write';
export const isGrepToolResult = event => event.toolName === 'grep';
export const isFindToolResult = event => event.toolName === 'find';
export const isLsToolResult = event => event.toolName === 'ls';

export function parseFrontmatter(content) {
  const normalized = content.replace(/^\uFEFF/, '').replace(/\r\n/g, '\n').replace(/\r/g, '\n');
  const end = normalized.startsWith('---') ? normalized.indexOf('\n---', 3) : -1;
  if (end < 0) return { frontmatter: {}, body: normalized };
  const yaml = normalized.slice(4, end);
  return { frontmatter: yaml ? (parse(yaml) ?? {}) : {}, body: normalized.slice(end + 4).trim() };
}
export const stripFrontmatter = content => parseFrontmatter(content).body;

const mutationQueues = new Map();
let registrationQueue = Promise.resolve();
export async function withFileMutationQueue(filePath, operation) {
  const registration = registrationQueue.then(async () => {
    let key = resolve(filePath);
    try { key = await realpath(key); }
    catch (error) { if (!['ENOENT', 'ENOTDIR'].includes(error?.code)) throw error; }
    const previous = mutationQueues.get(key) ?? Promise.resolve();
    let release;
    const next = new Promise(done => { release = done; });
    const tail = previous.then(() => next);
    mutationQueues.set(key, tail);
    return { key, previous, tail, release };
  });
  registrationQueue = registration.then(() => undefined, () => undefined);
  const { key, previous, tail, release } = await registration;
  await previous;
  try { return await operation(); }
  finally { release(); if (mutationQueues.get(key) === tail) mutationQueues.delete(key); }
}

export const DEFAULT_MAX_LINES = 2000;
export const DEFAULT_MAX_BYTES = 50 * 1024;
export function formatSize(bytes) {
  return bytes < 1024 ? `${bytes}B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)}KB` : `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}
export function truncateLine(line, maxChars = 500) {
  return line.length <= maxChars ? { text: line, wasTruncated: false } : { text: `${line.slice(0, maxChars)}... [truncated]`, wasTruncated: true };
}
function truncate(content, options, tail) {
  const maxLines = options.maxLines ?? DEFAULT_MAX_LINES, maxBytes = options.maxBytes ?? DEFAULT_MAX_BYTES;
  const lines = content.length ? content.split('\n') : [];
  if (content.endsWith('\n')) lines.pop();
  const totalBytes = Buffer.byteLength(content), totalLines = lines.length;
  const result = { content, truncated: false, truncatedBy: null, totalLines, totalBytes,
    outputLines: totalLines, outputBytes: totalBytes, lastLinePartial: false, firstLineExceedsLimit: false, maxLines, maxBytes };
  if (totalLines <= maxLines && totalBytes <= maxBytes) return result;
  if (!tail && Buffer.byteLength(lines[0]) > maxBytes) {
    return { ...result, content: '', truncated: true, truncatedBy: 'bytes', outputLines: 0, outputBytes: 0, firstLineExceedsLimit: true };
  }
  const output = [];
  let bytes = 0, truncatedBy = 'lines', lastLinePartial = false;
  for (let count = 0; count < lines.length && output.length < maxLines; count++) {
    const line = lines[tail ? lines.length - count - 1 : count];
    const length = Buffer.byteLength(line) + (output.length ? 1 : 0);
    if (bytes + length > maxBytes) {
      truncatedBy = 'bytes';
      if (tail && !output.length) {
        const buffer = Buffer.from(line);
        let start = buffer.length - maxBytes;
        while (start < buffer.length && (buffer[start] & 0xc0) === 0x80) start++;
        output.push(buffer.subarray(start).toString('utf8'));
        bytes = Buffer.byteLength(output[0]); lastLinePartial = true;
      }
      break;
    }
    if (tail) output.unshift(line); else output.push(line);
    bytes += length;
  }
  if (output.length >= maxLines && bytes <= maxBytes) truncatedBy = 'lines';
  const text = output.join('\n');
  return { ...result, content: text, truncated: true, truncatedBy, outputLines: output.length,
    outputBytes: Buffer.byteLength(text), lastLinePartial };
}
export const truncateHead = (content, options = {}) => truncate(content, options, false);
export const truncateTail = (content, options = {}) => truncate(content, options, true);

function bashExecutionToText(message) {
  let text = `Ran \`${message.command}\`\n` + (message.output ? `\`\`\`\n${message.output}\n\`\`\`` : '(no output)');
  if (message.cancelled) text += '\n\n(command cancelled)';
  else if (message.exitCode != null && message.exitCode !== 0) text += `\n\nCommand exited with code ${message.exitCode}`;
  if (message.truncated && message.fullOutputPath) text += `\n\n[Output truncated. Full output: ${message.fullOutputPath}]`;
  return text;
}
export function convertToLlm(messages) {
  return messages.flatMap(message => {
    const { timestamp } = message;
    switch (message.role) {
      case 'bashExecution': return message.excludeFromContext ? [] : [{ role: 'user', content: [{ type: 'text', text: bashExecutionToText(message) }], timestamp }];
      case 'custom': return [{ role: 'user', content: typeof message.content === 'string' ? [{ type: 'text', text: message.content }] : message.content, timestamp }];
      case 'branchSummary': return [{ role: 'user', content: [{ type: 'text', text: `The following is a summary of a branch that this conversation came back from:\n\n<summary>\n${message.summary}</summary>` }], timestamp }];
      case 'compactionSummary': return [{ role: 'user', content: [{ type: 'text', text: `The conversation history before this point was compacted into the following summary:\n\n<summary>\n${message.summary}\n</summary>` }], timestamp }];
      case 'system': case 'user': case 'assistant': case 'toolResult': return [message];
      default: return [];
    }
  });
}
const contentText = (content, separator = '\n') => typeof content === 'string' ? content : content.filter(part => part.type === 'text').map(part => part.text).join(separator);
export function serializeConversation(messages) {
  const parts = [];
  for (const message of messages) {
    if (message.role === 'user') {
      const text = contentText(message.content, ''); if (text) parts.push(`[User]: ${text}`);
    } else if (message.role === 'assistant') {
      const thinking = message.content.filter(part => part.type === 'thinking').map(part => part.thinking);
      const calls = message.content.filter(part => part.type === 'toolCall').map(part =>
        `${part.name}(${Object.entries(part.arguments).map(([key, value]) => `${key}=${JSON.stringify(value)}`).join(', ')})`);
      if (thinking.length) parts.push(`[Assistant thinking]: ${thinking.join('\n')}`);
      if (message.content.some(part => part.type === 'text')) parts.push(`[Assistant]: ${contentText(message.content)}`);
      if (calls.length) parts.push(`[Assistant tool calls]: ${calls.join('; ')}`);
    } else if (message.role === 'toolResult') {
      const text = contentText(message.content, '');
      if (text) parts.push(`[Tool result]: ${text.length <= 2000 ? text : `${text.slice(0, 2000)}\n\n[... ${text.length - 2000} more characters truncated]`}`);
    }
  }
  return parts.join('\n\n');
}

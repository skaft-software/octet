import { bounded, fields, invalid, rpcError, unsupported } from './errors.mjs';

// Command/attachment parsing adapted from Pi 1.0 (581e7ba78141a4d8b61cc9d11b8b22ae7e59195e),
// packages/tui/src/{autocomplete,utils}.ts. Copyright (c) 2025 Mario Zechner.
// MIT, see ../LICENSE.pi. No filesystem/path completion or Pi runtime is started.
const delimiters = new Set([' ', '\t', '"', "'", '=']);
const wrappers = { '(': ')', '[': ']', '{': '}', '<': '>', '`': '`' };
const cjk = /[\p{Script_Extensions=Han}\p{Script_Extensions=Hiragana}\p{Script_Extensions=Katakana}\p{Script_Extensions=Hangul}\p{Script_Extensions=Bopomofo}]/u;
const separator = new RegExp(`(?:\\s|(?=\\p{Punctuation})${cjk.source}|[，．：；！？（）［］｛｝“”‘’…—])`, 'u');
const boundary = new RegExp(`(?:^|${separator.source})$`, 'u');
function atAttachment(text) {
  let quoted = false, quoteStart = -1;
  for (let i = 0; i < text.length; i++) if (text[i] === '"') {
    quoted = !quoted;
    if (quoted) quoteStart = i;
  }
  if (quoted && quoteStart > 0 && text[quoteStart - 1] === '@') {
    let start = quoteStart - 1;
    while (start > 0 && wrappers[text[start - 1]]) start--;
    if (delimiters.has(text[start - 1]) || boundary.test(text.slice(0, start))) return true;
  }
  let last = -1, index = 0;
  for (const char of text) {
    index += char.length;
    if (delimiters.has(char) || separator.test(char)) last = index - 1;
  }
  let token = last === -1 ? text : text.slice(last + 1);
  while (token.length && wrappers[token[0]] && !token.includes(wrappers[token[0]], 1)) token = token.slice(1);
  return token.startsWith('@');
}

/** Translate a native UTF-8 byte cursor; keep Pi's complete raw argument prefix. */
export function commandArgumentRequest(params) {
  fields(params, ['text', 'cursor', 'revision'], 'autocomplete request');
  const text = bounded(params.text, 'autocomplete editor text', 262144, { controls: true });
  if (/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/u.test(text)) invalid('autocomplete editor text contains terminal controls');
  const bytes = Buffer.from(text);
  if (!Number.isSafeInteger(params.cursor) || params.cursor < 0 || params.cursor > bytes.length
      || params.cursor < bytes.length && (bytes[params.cursor] & 0xc0) === 0x80) invalid('autocomplete cursor must be a UTF-8 byte boundary');
  if (!Number.isSafeInteger(params.revision) || params.revision < 0) invalid('autocomplete revision');
  const before = bytes.subarray(0, params.cursor).toString('utf8');
  const line = before.slice(before.lastIndexOf('\n') + 1);
  const command = line.trimStart(), space = command.indexOf(' ');
  if (!command.startsWith('/') || space < 0 || atAttachment(line)) return null;
  return { name: command.slice(1, space), prefix: command.slice(space + 1),
    afterCursor: bytes.subarray(params.cursor).toString('utf8') };
}

function plain(text, label) {
  bounded(text, label, 1024, { controls: true });
  if (/[\x00-\x1f\x7f-\x9f]/u.test(text)) unsupported(label, 'native autocomplete requires single-line plain text');
  return text;
}

async function cancellable(promise, signal) {
  signal.throwIfAborted();
  let abort;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      abort = () => reject(signal.reason);
      signal.addEventListener('abort', abort, { once: true });
    })]);
  } finally { signal.removeEventListener('abort', abort); }
}

export async function commandCompletions(runtime, params, store) {
  runtime.require('autocomplete');
  if (!runtime.autocompleteRegistration) unsupported('command completions', 'no registered completion chain');
  const query = commandArgumentRequest(params);
  await cancellable(runtime.autocompleteRegistration, store.controller.signal);
  store.controller.signal.throwIfAborted();
  const command = query && runtime.commands.get(query.name);
  if (!command?.definition.getArgumentCompletions) return { prefix: '', items: [] };
  const scope = { ...store, factory: command.factory };
  const result = await cancellable(runtime.scope.run(scope, () => command.definition.getArgumentCompletions(query.prefix)), store.controller.signal);
  store.controller.signal.throwIfAborted();
  // Pi 1.0 treats null, empty arrays and non-array returns as no suggestions.
  if (!Array.isArray(result) || !result.length) return { prefix: '', items: [] };
  if (result.length > 32) throw rpcError(-32602, 'bounds_exceeded autocomplete items');
  const prefix = plain(query.prefix, 'autocomplete prefix');
  const items = Array.from(result, item => {
    fields(item, ['value', 'label', 'description'], 'autocomplete item');
    const value = plain(item.value, 'autocomplete value'), label = plain(item.label, 'autocomplete label');
    // Pi applyCompletion can consume a quote after the cursor or leave its
    // cursor inside a quoted directory. The native suffix-only wire cannot.
    if ((prefix.startsWith('"') || prefix.startsWith('@"')) && value.endsWith('"') && query.afterCursor.startsWith('"')) {
      unsupported('autocomplete replacement range', 'native wire cannot consume a quote after the cursor');
    }
    if (label.endsWith('/') && value.endsWith('"')) unsupported('autocomplete cursor offset', 'native wire places the cursor after the whole value');
    // A raw argument prefix may start with an earlier @ token although the
    // current token is not an attachment. Pi applyCompletion still takes its
    // attachment branch and appends a space for a non-directory selection.
    const replacement = prefix.startsWith('@') && !label.endsWith('/') ? plain(`${value} `, 'autocomplete value') : value;
    return { value: replacement, label, ...(item.description === undefined ? {} : { description: plain(item.description, 'autocomplete description') }) };
  });
  return { prefix, items };
}

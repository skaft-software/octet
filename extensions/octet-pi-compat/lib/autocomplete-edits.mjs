import { bounded, fields, invalid, rpcError, unsupported } from './errors.mjs';

const editorLimit = 262144;
const controls = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/u;
const allControls = /[\x00-\x1f\x7f-\x9f]/u;
export function completionText(value, label, edits = false, limit = 1024) {
  bounded(value, label, limit, { controls: true });
  if ((edits ? controls : allControls).test(value)) unsupported(label, edits
    ? 'only LF, TAB and CR controls are representable' : 'native autocomplete requires single-line plain text');
  return value;
}
function scalarBoundary(text, at) {
  return Number.isSafeInteger(at) && at >= 0 && at <= text.length
    && !(at > 0 && at < text.length && /[\uD800-\uDBFF]/u.test(text[at - 1]) && /[\uDC00-\uDFFF]/u.test(text[at]));
}

/** The host speaks UTF-8 bytes; Pi speaks line numbers and UTF-16 columns. Never round either. */
export function autocompleteSnapshot(params) {
  fields(params, ['text', 'cursor', 'revision'], 'autocomplete request');
  const text = bounded(params.text, 'autocomplete editor text', editorLimit, { controls: true });
  if (controls.test(text)) invalid('autocomplete editor text contains terminal controls');
  const bytes = Buffer.from(text);
  if (!Number.isSafeInteger(params.cursor) || params.cursor < 0 || params.cursor > bytes.length
      || params.cursor < bytes.length && (bytes[params.cursor] & 0xc0) === 0x80) invalid('autocomplete cursor must be a UTF-8 byte boundary');
  if (!Number.isSafeInteger(params.revision) || params.revision < 0) invalid('autocomplete revision');
  const before = bytes.subarray(0, params.cursor).toString('utf8');
  const lines = text.split('\n'), cursorLine = before.split('\n').length - 1;
  return { ...params, lines, cursorLine, cursorCol: before.length - before.lastIndexOf('\n') - 1, index: before.length };
}

export function piPosition(lines, cursorLine, cursorCol) {
  if (!Array.isArray(lines) || !lines.length || lines.length > editorLimit + 1) invalid('autocomplete lines');
  let bytes = lines.length - 1;
  for (let i = 0; i < lines.length; i++) {
    bounded(lines[i], 'autocomplete line', editorLimit, { controls: true });
    bytes += Buffer.byteLength(lines[i]);
    if (bytes > editorLimit) throw rpcError(-32602, 'bounds_exceeded autocomplete editor text');
    if (lines[i].includes('\n') || controls.test(lines[i])) invalid('autocomplete line contains an invalid control or embedded newline');
  }
  if (!Number.isSafeInteger(cursorLine) || cursorLine < 0 || cursorLine >= lines.length
      || !scalarBoundary(lines[cursorLine], cursorCol)) invalid('autocomplete cursor must be an exact UTF-16 scalar boundary');
  const text = lines.join('\n');
  bounded(text, 'autocomplete editor text', editorLimit, { controls: true });
  let index = cursorCol;
  for (let i = 0; i < cursorLine; i++) index += lines[i].length + 1;
  return { text, index };
}

/** Pi 1.0 CombinedAutocompleteProvider.applyCompletion, without filesystem/runtime dependencies.
 * Adapted from 581e7ba78141a4d8b61cc9d11b8b22ae7e59195e, MIT ../LICENSE.pi.
 */
export function applyPiCompletion(lines, cursorLine, cursorCol, item, prefix) {
  const currentLine = lines[cursorLine] || '';
  const beforePrefix = currentLine.slice(0, cursorCol - prefix.length);
  const afterCursor = currentLine.slice(cursorCol);
  const adjustedAfter = (prefix.startsWith('"') || prefix.startsWith('@"'))
    && item.value.endsWith('"') && afterCursor.startsWith('"') ? afterCursor.slice(1) : afterCursor;
  const slash = prefix.startsWith('/') && beforePrefix.trim() === '' && !prefix.slice(1).includes('/');
  const directory = item.label.endsWith('/');
  const suffix = slash || prefix.startsWith('@') && !directory ? ' ' : '';
  const value = (slash ? '/' : '') + item.value + suffix;
  const cursorOffset = !slash && directory && item.value.endsWith('"') ? item.value.length - 1 : item.value.length;
  const newLines = [...lines]; newLines[cursorLine] = beforePrefix + value + adjustedAfter;
  return { lines: newLines, cursorLine, cursorCol: beforePrefix.length + (slash ? 1 : 0) + cursorOffset + suffix.length };
}

function editForResult(snapshot, result, hintStart) {
  fields(result, ['lines', 'cursorLine', 'cursorCol'], 'autocomplete applyCompletion result');
  const next = piPosition(result.lines, result.cursorLine, result.cursorCol);
  const old = snapshot.text;
  // A single replacement can encode arbitrary multiline edits as long as the
  // unchanged prefix/suffix survive and the resulting cursor is inside it.
  let start = 0;
  const commonLimit = Math.min(hintStart, next.index);
  while (start < commonLimit && old[start] === next.text[start]) start++;
  if (!scalarBoundary(old, start) || !scalarBoundary(next.text, start)) start--;
  let suffix = 0;
  const suffixLimit = Math.min(old.length - snapshot.index, next.text.length - next.index);
  while (suffix < suffixLimit && old[old.length - suffix - 1] === next.text[next.text.length - suffix - 1]) suffix++;
  if (!scalarBoundary(old, old.length - suffix) || !scalarBoundary(next.text, next.text.length - suffix)) suffix--;
  return { start, value: next.text.slice(start, next.text.length - suffix),
    after: Buffer.byteLength(old.slice(snapshot.index, old.length - suffix)), cursor: next.index - start };
}

/** Compute edits now; the native host applies one only on explicit, snapshot-fenced acceptance. */
export function completionResponse(snapshot, suggestions, provider, edits = false) {
  if (suggestions === null || suggestions === undefined) return { prefix: '', items: [] };
  fields(suggestions, ['prefix', 'items'], 'autocomplete suggestions');
  if (!Array.isArray(suggestions.items)) invalid('autocomplete items must be an array');
  if (!suggestions.items.length) return { prefix: '', items: [] };
  if (suggestions.items.length > 32) throw rpcError(-32602, 'bounds_exceeded autocomplete items');
  const hint = completionText(suggestions.prefix, 'autocomplete prefix', edits);
  if (!snapshot.text.slice(0, snapshot.index).endsWith(hint)) invalid('autocomplete prefix must be the exact suffix before the cursor');
  const hintStart = snapshot.index - hint.length;
  // Snapshot before invoking extension code: a callback must not lengthen our bounded loop.
  const choices = Array.from(suggestions.items).map(item => {
    fields(item, ['value', 'label', 'description'], 'autocomplete item');
    completionText(item.value, 'autocomplete value', edits);
    completionText(item.label, 'autocomplete label');
    if (item.description !== undefined) completionText(item.description, 'autocomplete description');
    const applied = provider.applyCompletion([...snapshot.lines], snapshot.cursorLine, snapshot.cursorCol, item, hint);
    if (applied?.then) unsupported('autocomplete applyCompletion', 'Pi requires a synchronous edit result');
    return { item, edit: editForResult(snapshot, applied, hintStart) };
  });
  const start = Math.min(...choices.map(({ edit }) => edit.start));
  const prefix = completionText(snapshot.text.slice(start, snapshot.index), 'autocomplete prefix', edits);
  const items = choices.map(({ item, edit }) => {
    const head = snapshot.text.slice(start, edit.start);
    const value = completionText(head + edit.value, 'autocomplete value', edits);
    const cursor = Buffer.byteLength(head + edit.value.slice(0, edit.cursor));
    if (!edits && edit.after) unsupported('autocomplete replacement range', 'autocomplete_edit_v1 was not negotiated');
    if (!edits && cursor !== Buffer.byteLength(value)) unsupported('autocomplete cursor offset', 'autocomplete_edit_v1 was not negotiated');
    // Other applyCompletion calls can mutate shared item objects. Validate the
    // actual outbound metadata again after all callbacks have finished.
    completionText(item.label, 'autocomplete label');
    if (item.description !== undefined) completionText(item.description, 'autocomplete description');
    return { value, label: item.label, ...(item.description === undefined ? {} : { description: item.description }),
      ...(edit.after ? { replace_after_bytes: edit.after } : {}),
      ...(cursor !== Buffer.byteLength(value) ? { cursor_offset_bytes: cursor } : {}) };
  });
  return { prefix, items };
}

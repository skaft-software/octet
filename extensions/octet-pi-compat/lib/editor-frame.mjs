// Read-only layout of a native observation; no editable text, caret or history.
// Pi packages/tui/src/components/editor.ts:526-620 lays out local observations
// during paint. Native atoms retain paste boundaries and native navigation uses
// the same last-painted geometry, supplied outside render by the facade.
import { visibleWidth } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';

const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
const whitespace = text => /^\p{White_Space}+$/u.test(text);

export function editorFrame(snapshot, width, rows, scroll) {
  const text = snapshot.text, padding = Math.min(snapshot.padding_x, Math.floor((width - 1) / 2));
  const contentWidth = width - 2 * padding, layoutWidth = Math.max(1, contentWidth - (padding ? 0 : 1));
  const atoms = snapshot.atoms.map(([start, end]) => ({ start, end, width: visibleWidth(text.slice(start, end)) }));
  const layout = [];
  let base = 0, atomIndex = 0;
  // Match native TextEditor's consumed separator ranges so cursor navigation and
  // paint agree even when the caret sits in soft-wrap whitespace.
  for (const logical of text.split('\n')) {
    const segments = [...segmenter.segment(logical)].map(({ segment, index }) => {
      const start = base + index;
      while (atoms[atomIndex]?.end <= start) atomIndex++;
      const atom = atoms[atomIndex];
      return { text: segment, start, width: visibleWidth(segment), atom: atom?.start <= start ? atom : undefined };
    });
    if (!segments.length) layout.push({ start: base, end: base, visibleEnd: base });
    let first = 0;
    while (first < segments.length) {
      const start = segments[first].start;
      let columns = 0, hardEnd = segments.length, wordBreak, sawText = false, overflowWhitespace = false;
      for (let i = first; i < segments.length; i++) {
        const s = segments[i];
        if (i > first && (s.atom?.start === s.start && columns + s.atom.width > layoutWidth || columns + s.width > layoutWidth)) {
          hardEnd = i;
          overflowWhitespace = !(s.atom?.start === s.start && columns + s.atom.width > layoutWidth) && whitespace(s.text);
          break;
        }
        columns += s.width;
        if (whitespace(s.text) && !(s.atom && s.atom.width <= layoutWidth)) {
          if (sawText) wordBreak = wordBreak?.end === i ? { start: wordBreak.start, end: i + 1 } : { start: i, end: i + 1 };
        } else sawText = true;
      }
      if (hardEnd === segments.length) {
        layout.push({ start, end: base + logical.length, visibleEnd: base + logical.length });
        break;
      }
      let next = hardEnd, visibleEnd = segments[hardEnd].start;
      if (overflowWhitespace) {
        while (next < segments.length && whitespace(segments[next].text)) next++;
      } else if (wordBreak) {
        next = wordBreak.end;
        visibleEnd = segments[wordBreak.start].start;
      }
      const end = segments[next]?.start ?? base + logical.length;
      layout.push({ start, end, visibleEnd });
      first = next;
    }
    base += logical.length + 1;
  }
  const cursor = snapshot.cursor_offset;
  const cursorRow = Math.max(0, layout.findLastIndex(line => line.start <= cursor));
  const limit = Math.max(5, Math.min(snapshot.autocomplete ? 233 : 254, Math.floor(rows * 0.3)));
  if (cursorRow < scroll) scroll = cursorRow;
  if (cursorRow >= scroll + limit) scroll = cursorRow + 1 - limit;
  scroll = Math.min(scroll, Math.max(0, layout.length - limit));
  const end = Math.min(scroll + limit, layout.length);
  const lines = layout.slice(scroll, end).map((line, i) => {
    if (scroll + i !== cursorRow) return { text: text.slice(line.start, line.visibleEnd) };
    const offset = Math.max(line.start, Math.min(cursor, line.visibleEnd));
    const tail = text.slice(offset, line.visibleEnd), caret = segmenter.segment(tail)[Symbol.iterator]().next().value?.segment ?? ' ';
    return { before: text.slice(line.start, offset), cursor: caret, after: tail.slice(caret.length) };
  });
  return { lines, padding, content_width: contentWidth, hidden_above: scroll,
    hidden_below: layout.length - end, autocomplete: snapshot.autocomplete, scroll };
}

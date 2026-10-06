// Editing state lives exclusively in the foreground Rust TextEditor service.
// This facade retains component callbacks, styles and the opaque native handle.
import { nativeEditorCall, registerNativeEditor } from './remote-ui.mjs';
import { bounded, invalid } from './errors.mjs';
import { SelectList } from '../node_modules/@earendil-works/pi-tui/dist/components/select-list.js';
import { getKeybindings } from '../node_modules/@earendil-works/pi-tui/dist/keybindings.js';
import { decodePrintableKey, isKeyRelease, matchesKey } from '../node_modules/@earendil-works/pi-tui/dist/keys.js';
import { visibleWidth, truncateToWidth } from '../node_modules/@earendil-works/pi-tui/dist/utils.js';

const CURSOR_MARKER = '\x1b_pi:c\x07';
const keyActions = [
  ['tui.input.copy', 'ignore'], ['tui.editor.undo', 'undo'],
  ['tui.editor.deleteToLineEnd', 'delete_to_line_end'], ['tui.editor.deleteToLineStart', 'delete_to_line_start'],
  ['tui.editor.deleteWordBackward', 'delete_word_backward'], ['tui.editor.deleteWordForward', 'delete_word_forward'],
  ['tui.editor.deleteCharBackward', 'backspace'], ['tui.editor.deleteCharForward', 'delete'],
  ['tui.editor.yank', 'yank'], ['tui.editor.yankPop', 'yank_pop'],
  ['tui.editor.historyPrevious', 'history_previous'], ['tui.editor.historyNext', 'history_next'],
  ['tui.editor.cursorLineStart', 'home'], ['tui.editor.cursorLineEnd', 'end'],
  ['tui.editor.cursorWordLeft', 'word_left'], ['tui.editor.cursorWordRight', 'word_right'],
  ['tui.input.newLine', 'newline'], ['tui.input.submit', 'submit'],
  ['tui.editor.cursorUp', 'up'], ['tui.editor.cursorDown', 'down'],
  ['tui.editor.cursorRight', 'right'], ['tui.editor.cursorLeft', 'left'],
  ['tui.editor.pageUp', 'page_up'], ['tui.editor.pageDown', 'page_down'],
  ['tui.editor.jumpForward', 'jump_forward'], ['tui.editor.jumpBackward', 'jump_backward'],
];
const integer = (value, fallback = 0) => Number.isFinite(value) ? Math.max(0, Math.floor(value)) : fallback;
function scrollBorder(direction, count, width) {
  const label = ` ${direction} ${count} more `;
  if (label.length + 2 > width) {
    const indicator = '───' + label;
    if (indicator.length <= width) return indicator + '─'.repeat(width - indicator.length);
    const ellipsis = '...'.slice(0, width);
    return indicator.slice(0, width - ellipsis.length) + ellipsis;
  }
  const left = Math.floor((width - label.length) / 2);
  return '─'.repeat(left) + label + '─'.repeat(width - label.length - left);
}

export class Editor {
  #call;
  #id;
  #disposed = false;
  #provider;
  #theme;
  #controller;
  #debounceTimer;
  #references;
  #binding;
  focused = false;
  disableSubmit = false;
  onSubmit;
  onChange;
  constructor(tui, theme, options = {}) {
    this.tui = tui;
    this.borderColor = theme.borderColor;
    this.#theme = theme;
    this.#call = nativeEditorCall(tui);
    const result = this.#call({op: 'create', padding_x: integer(options.paddingX),
      autocomplete_max_visible: Math.max(3, Math.min(20, integer(options.autocompleteMaxVisible ?? 5, 5)))});
    if (typeof result.editor_id !== 'string' || !/^[A-Za-z0-9_.-]{1,64}$/.test(result.editor_id)) invalid('native editor handle');
    this.#id = result.editor_id;
    this.#binding = registerNativeEditor(this, this.#call, this.#id);
  }
  #request(operation) {
    if (this.#disposed) invalid('native editor disposed');
    return this.#call(operation, this.#id);
  }
  #read(field) { return this.#request({op: 'read', field}).value; }
  #mutate(operation) {
    let reply = this.#request(operation);
    if (reply.apply_completion) return this.#applySelected(reply.submit_after);
    if (reply.change !== null && reply.change !== undefined) this.onChange?.(reply.change);
    else if (reply.cursor_changed) this.#binding.checkpoint?.();
    if (reply.submit !== null && reply.submit !== undefined) this.onSubmit?.(reply.submit);
    this.tui.requestRender();
    return reply;
  }
  getText() { return this.#read('text'); }
  getExpandedText() { return this.#read('expanded'); }
  getLines() { return this.#read('lines'); }
  getCursor() { return this.#read('cursor'); }
  getPaddingX() { return this.#read('padding'); }
  setPaddingX(value) { this.#mutate({op: 'padding', value: integer(value)}); }
  getAutocompleteMaxVisible() { return this.#read('autocomplete_max_visible'); }
  setAutocompleteMaxVisible(value) {
    this.#mutate({op: 'autocomplete_max_visible', value: Math.max(3, Math.min(20, integer(value, 5)))});
  }
  setText(text) { this.#mutate({op: 'set_text', text: bounded(text, 'editor text', 262144, {controls: true})}); }
  insertTextAtCursor(text) { this.#mutate({op: 'insert', text: bounded(text, 'editor insert', 262144, {controls: true})}); }
  addToHistory(text) { this.#request({op: 'add_history', text: bounded(text, 'editor history', 262144, {controls: true})}); }
  // Read-only native observations retain existing checkpoint qualification.
  // Neither collection is an editable JavaScript paste or undo ledger.
  get pastes() { return new Map(this.#read('pastes')); }
  get undoStack() { return this.#read('undo_history'); }
  setAutocompleteProvider(provider) {
    this.#cancelRequest();
    this.#request({op: 'configure_autocomplete', trigger_characters: provider.triggerCharacters ?? []});
    this.#provider = provider;
    this.#references = undefined;
  }
  #cancelRequest() {
    this.#controller?.abort(); this.#controller = undefined;
    clearTimeout(this.#debounceTimer); this.#debounceTimer = undefined;
  }
  #query(force = false) {
    if (!this.#provider) return;
    this.#cancelRequest();
    const {query} = this.#request({op: 'query', force});
    if (!query) return;
    if (query.force && this.#provider.shouldTriggerFileCompletion && !this.#provider.shouldTriggerFileCompletion(query.lines, query.cursor.line, query.cursor.col)) return;
    const controller = this.#controller = new AbortController();
    const run = async () => {
      try {
        const result = await this.#provider.getSuggestions(query.lines, query.cursor.line, query.cursor.col,
          {signal: controller.signal, force: query.force});
        if (controller.signal.aborted || this.#disposed || !this.#call.isAlive()) return;
        const items = result?.items ?? [];
        const receipt = this.#request({op: 'suggestions', query_id: query.id, prefix: result?.prefix ?? '',
          items: items.map((item, id) => ({id, value: item.value, label: item.label, ...(item.description === undefined ? {} : {description: item.description})}))});
        if (!receipt.accepted) return;
        this.#references = {queryId: query.id, items};
        if (receipt.apply) this.#applySelected(false);
        this.tui.requestRender();
      } catch (_error) {
        if (!controller.signal.aborted && !this.#disposed && this.#call.isAlive()) {
          this.#request({op: 'cancel_autocomplete'});
          this.tui.requestRender();
        }
      }
    };
    if (query.delay) this.#debounceTimer = setTimeout(run, query.delay);
    else void run();
  }
  #applySelected(submit) {
    const {selection} = this.#request({op: 'selection'});
    if (!selection) return;
    if (selection.query_id !== this.#references?.queryId) invalid('native completion callback handle');
    const item = this.#references.items[selection.id];
    if (!item) invalid('native completion item handle');
    const result = this.#provider.applyCompletion(selection.lines, selection.cursor.line, selection.cursor.col, item, selection.prefix);
    this.#cancelRequest();
    return this.#mutate({op: 'complete', revision: selection.revision, submit,
      lines: result.lines, cursor_line: result.cursorLine, cursor_col: result.cursorCol});
  }
  isShowingAutocomplete() { return this.#read('autocomplete'); }
  invalidate() {}
  renderTopBorder(width, hidden) { return this.borderColor(hidden ? scrollBorder('↑', hidden, width) : '─'.repeat(width)); }
  renderBottomBorder(width, hidden) { return this.borderColor(hidden ? scrollBorder('↓', hidden, width) : '─'.repeat(width)); }
  render(width) {
    const frame = this.#request({op: 'render', width, rows: this.tui.terminal.rows});
    const pad = ' '.repeat(frame.padding);
    const lines = frame.lines.map(row => {
      const text = row.text ?? (row.before + (this.focused ? CURSOR_MARKER : '') + '\x1b[7m' + row.cursor + '\x1b[0m' + row.after);
      const cells = visibleWidth(text);
      const right = cells > frame.content_width ? pad.slice(1) : pad;
      return truncateToWidth(pad + text + ' '.repeat(Math.max(0, frame.content_width - cells)) + right, width, '');
    });
    const result = [this.renderTopBorder(width, frame.hidden_above), ...lines, this.renderBottomBorder(width, frame.hidden_below)];
    if (frame.autocomplete) {
      const list = new SelectList(frame.autocomplete.items, frame.autocomplete.max_visible, this.#theme.selectList);
      list.setSelectedIndex(frame.autocomplete.selected);
      result.push(...list.render(frame.content_width).map(line => pad + line + ' '.repeat(Math.max(0, frame.content_width - visibleWidth(line))) + pad));
    }
    return result;
  }
  handleInput(data) {
    bounded(data, 'editor input', 262156, {controls: true});
    if (isKeyRelease(data)) return;
    const kb = getKeybindings();
    let action = keyActions.find(([key]) => kb.matches(data, key))?.[1] ?? null;
    if (matchesKey(data, 'shift+backspace')) action = 'backspace';
    if (matchesKey(data, 'shift+delete')) action = 'delete';
    const tab = kb.matches(data, 'tui.input.tab');
    const menuAction = [['tui.select.cancel', 'cancel'], ['tui.select.up', 'up'], ['tui.select.down', 'down'], ['tui.select.confirm', 'submit']].find(([key]) => kb.matches(data, key))?.[1] ?? (tab ? 'tab' : null);
    const printable = action === null && !data.includes('\x1b[200~') ? decodePrintableKey(data) : undefined;
    const reply = this.#mutate({op: 'input', data: printable ?? data, action, menu_action: menuAction, disable_submit: this.disableSubmit});
    if (tab && !reply?.menu_handled && !reply?.apply_completion && !reply?.change) this.#query(true);
    else if (!reply?.menu_handled && reply?.submit == null && menuAction !== 'cancel' && !data.includes('\x1b[200~')) this.#query();
  }
  handleMouse() { return undefined; }
  dispose() {
    if (this.#disposed) return;
    this.#cancelRequest();
    this.#call({op: 'dispose'}, this.#id);
    this.#disposed = true;
  }
}

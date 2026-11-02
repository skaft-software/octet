import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { RemoteTUI, RemoteUI } from '../lib/remote-ui.mjs';
import { Editor } from '../lib/editor.mjs';

// A stub native TextEditor service: the real Editor facade and RemoteUI.close
// run unchanged; only the ui/chrome transport is simulated.
function harness() {
  const owner = { session_id: 's', extension_instance_id: 'i', process_generation: 1 };
  const state = { alive: true, owner }, calls = [], errors = [];
  const text = new Map();
  const runtime = {
    stopping: false, scope: new AsyncLocalStorage(), timers: { surface() {} },
    require() {}, assertOwner(store) { if (!store.state.alive) throw Error('not_foreground_owner'); },
    backgroundError(error) { errors.push(error); },
    transport: { requestSync(method, params) {
      const { operation, editor_id: id } = params.chrome; calls.push(operation.op);
      if (operation.op === 'create') { const editorId = `e${text.size + 1}`; text.set(editorId, ''); return { editor_id: editorId }; }
      if (operation.op === 'set_text') { text.set(id, operation.text); return { change: operation.text }; }
      if (operation.op === 'read') {
        const value = text.get(id);
        return { value: { text: value, expanded: value, lines: value.split('\n'), cursor: { line: 0, col: value.length }, padding: 0, autocomplete_max_visible: 5 }[operation.field] };
      }
      return {};
    } },
  };
  const ui = new RemoteUI(runtime);
  const store = { id: 1, state, live: true, controller: new AbortController() };
  const surface = { id: 'pi-1', runtime, store, closed: false, opened: true, placement: 'overlay', initialInputs: [], columns: 80, rows: 24, requestRender() {} };
  surface.tui = new RemoteTUI(surface);
  ui.surfaces.set(surface.id, surface);
  ui.releaseMount = async () => { calls.push('release'); };
  const theme = { borderColor: s => s };
  return { ui, surface, store, calls, errors, editor: () => runtime.scope.run(store, () => new Editor(surface.tui.facade, theme)) };
}

test('editors stay readable with their final state after the adapter resolves their custom UI', async () => {
  const { ui, surface, calls, errors, editor } = harness();
  const notes = editor(), inline = editor();
  notes.setText('kept notes'); inline.setText('typed answer');
  await ui.close(surface, { answer: 'done' });
  // Original rpiv-ask-user-question mirrors drafts and projects its view after done().
  assert.equal(notes.getExpandedText(), 'kept notes');
  assert.equal(notes.getText(), 'kept notes');
  assert.equal(inline.getText(), 'typed answer');
  assert.deepEqual(inline.getLines(), ['typed answer']);
  assert.deepEqual(inline.getCursor(), { line: 0, col: 12 });
  assert.deepEqual(errors, []);
  const before = calls.length;
  // Writes and unretained observations still refuse; nothing reaches the host.
  assert.throws(() => notes.setText('late'), /native editor surface retired/);
  assert.throws(() => notes.undoStack, /native editor surface retired/);
  assert.equal(notes.getText(), 'kept notes');
  assert.equal(calls.length, before);
  assert.ok(calls.indexOf('release') > calls.lastIndexOf('read'));
});

test('host-observed retirement retains nothing and reads still refuse', async () => {
  const { ui, surface, editor } = harness();
  const notes = editor(); notes.setText('draft');
  await ui.close(surface, undefined, true);
  assert.throws(() => notes.getText(), /native editor surface retired/);
});

// Native composer admission for a mounted Pi editor. Recovery checkpoints and
// frames remain RemoteUI's responsibility; this owns only submit/ACK lifetime.
import { fields, invalid, rpcError } from './errors.mjs';
import { hostKeybindings } from './theme.mjs';

export function bindEditorAdmission(remoteUI, surface, ordered) {
  const runtime = remoteUI.runtime, c = surface.component, store = surface.store;
  const editor = surface.editor, submit = c.onSubmit;
  c.onAction?.('app.clear', () => c.setText(''));
  // Pi Editor.submitValue destroys pastes and undo before calling onSubmit.
  // Defer that exact implementation (including subclass behavior) until ACK.
  // No draft reconstruction or shallow rollback can preserve its undo stack.
  const commitSubmit = typeof c.submitValue === 'function' && typeof c.getExpandedText === 'function'
    ? c.submitValue.bind(c) : null;
  if (commitSubmit) c.submitValue = () => c.onSubmit?.(c.getExpandedText().trim());
  const followUp = () => c.onSubmit?.(c.getExpandedText?.().trim() ?? c.getText(), true);
  if (c.onAction) c.onAction('app.message.followUp', followUp);
  else {
    const input = c.handleInput?.bind(c), keys = hostKeybindings(() => store.state.host.keybindings);
    c.handleInput = data => keys.matches(data, 'app.message.followUp') ? followUp() : input?.(data);
  }
  c.onSubmit = (value, followUp = false) => {
    if (surface.closed || editor.admitting || editor.submissions && editor.inputKind !== 'press') return;
    editor.admitting = true;
    const changeRevision = editor.changeRevision;
    const send = () => {
      const checkpoint = { surface_id: surface.id, mount_id: surface.mountId,
        input_revision: editor.inputRevision, checkpoint_revision: editor.checkpointRevision };
      return ordered(async () => {
        try {
          const result = await runtime.hostCall('composer/submit', {
            text: value, follow_up: followUp, editor_checkpoint: checkpoint,
          }, store);
          fields(result, ['accepted'], 'composer admission');
          if (typeof result.accepted !== 'boolean') invalid('composer admission requires accepted');
          if (surface.closed) return;
          runtime.assertOwner(store);
          if (result.accepted) {
            try { runtime.scope.run(store, () => {
              if (commitSubmit && editor.changeRevision === changeRevision) {
                const onSubmit = c.onSubmit;
                c.onSubmit = undefined;
                editor.mutating++;
                try { commitSubmit(); } finally { editor.mutating--; c.onSubmit = onSubmit; }
              }
              submit?.(value);
            }); } finally { editor.checkpoint(); }
          }
        } catch (error) {
          // Admission refusal is not a failed recovery checkpoint. Keep the
          // component, its pastes and undo alive, and report without retrying.
          runtime.backgroundError(error);
        } finally {
          editor.admitting = false;
          if (!surface.closed) {
            const inputs = surface.initialInputs.splice(0);
            for (const input of inputs) remoteUI.editorInput(surface, input);
            surface.requestRender();
          }
        }
      });
    };
    if (editor.submissions) {
      if (editor.submissions.length >= 128) throw rpcError(-32012, 'bounds_exceeded editor submissions');
      editor.submissions.push(send);
    } else { editor.checkpoint(); send(); }
  };
}

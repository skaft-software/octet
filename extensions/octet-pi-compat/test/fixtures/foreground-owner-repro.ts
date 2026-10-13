// Synthetic owner-routing fixture, not native foreground/UI acceptance.
export default function (pi) {
  let release, waiting = false;
  const calls = [];
  pi.on('session_start', (_event, ctx) => {
    ctx.ui.setWidget('owner-proof', ['synthetic foreground']);
  });
  pi.on('session_shutdown', () => {});
  pi.on('context', async (event, ctx) => {
    const proof = { session: ctx.sessionId, entries: ctx.sessionManager.getEntries().map(entry => entry.id), hasUI: ctx.hasUI };
    if (ctx.sessionManager.getSessionName() === 'append') pi.appendEntry('background-proof', { session: ctx.sessionId });
    for (const [name, attempt] of [
      ['chrome', () => ctx.ui.setWorkingMessage('synthetic owner check')],
      ['notify', () => ctx.ui.notify('must not escape background')],
      ['status', () => ctx.ui.setStatus('foreign', 'must not mutate')],
      ['composer', () => ctx.ui.setEditorText('must not mutate')],
    ]) {
      if (ctx.hasUI) continue;
      try { attempt(); proof[name] = 'accepted'; } catch (error) { proof[name] = error.message; }
    }
    calls.push(proof);
    if (ctx.sessionManager.getSessionName() === 'wait') {
      waiting = true;
      await new Promise(resolve => { release = resolve; });
      waiting = false;
      proof.after = ctx.sessionManager.getSessionId();
    }
    event.messages.push({ role: 'user', content: JSON.stringify(proof) });
  });
  pi.on('session_info_changed', (_event, ctx) => {
    ctx.ui.setWorkingMessage('synthetic owner check');
  });
  pi.registerCommand('release-owner', { handler() { release?.(); } });
  pi.registerCommand('inspect-owner', {
    handler(_args, ctx) {
      let entries, name;
      try { name = ctx.sessionManager.getSessionName(); } catch (error) { name = error.message; }
      try { entries = ctx.sessionManager.getEntries().length; }
      catch (error) { entries = error.message; }
      ctx.ui.notify(`owner-proof:${JSON.stringify({
        session: ctx.sessionId, name, entries, calls, waiting,
      })}`);
    },
  });
}

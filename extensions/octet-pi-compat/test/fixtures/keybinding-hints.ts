// Pi 1.0.2's keybinding-hint helpers against the host-supplied bindings.
import { keyHint, keyText, rawKeyHint } from '@earendil-works/pi-coding-agent';
export default pi => pi.registerCommand('hints', { handler: (_args, ctx) => {
  const report = { keyText: undefined, rawHint: rawKeyHint('ctrl+x', 'close'), hint: keyHint('tui.select.confirm', 'quit'), refused: false };
  try { report.keyText = keyText('tui.select.confirm'); } catch (error) { report.keyTextError = error.message; }
  try { keyText('not.a.real.action'); } catch (error) { report.refused = /host binding not supplied/.test(error.message); report.refusal = error.message; }
  ctx.ui.notify(JSON.stringify(report));
} });

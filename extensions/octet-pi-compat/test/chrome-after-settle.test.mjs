// Pi 1.5.0 ExtensionUIContext setters are void and outlive the handler that called them
// (examples/extensions/working-indicator.ts and working-message-test.ts call setWorkingIndicator from
// session_start). The host cancels a request still pending when its parent settles, which surfaced as
// "Pi compatibility error: request cancelled" / "session_start hook failed".
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

test('a chrome update made inside session_start is sent after the hook has replied', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'pi-chrome-settle-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.mjs');
  writeFileSync(entry, `export default pi => {
    pi.on('session_start', async (_event, ctx) => {
      ctx.ui.setWorkingIndicator({ frames: ['-', '+'], intervalMs: 80 });
      ctx.ui.setWorkingMessage('working');
      // The hook is still running when the update is delivered, as it is while the host starts up.
      await new Promise(resolve => setTimeout(resolve, 60));
    });
  };`);
  const h = launch(t, [entry]);
  await h.init();
  const started = await h.start();
  assert.ok(started.result, 'session_start replied');
  const chrome = await h.wait(frame => frame.method === 'ui/chrome' && frame.params.chrome.kind === 'working_indicator');
  assert.deepEqual(chrome.params.chrome.frames, ['-', '+']);
  const order = h.seen.map(frame => frame.id === started.id && !frame.method ? 'reply' : frame.method === 'ui/chrome' ? 'chrome' : '');
  assert.ok(order.indexOf('reply') >= 0 && order.indexOf('reply') < order.indexOf('chrome'), `chrome sent after the reply: ${order.filter(Boolean)}`);
});

// A slow session_start (like a heavy extension warming up) followed by a quit.
import { appendFileSync } from 'node:fs';
export default pi => {
  const log = line => appendFileSync(process.env.STARTUP_QUIT_LOG, `${line}\n`);
  pi.on('session_start', async () => { log('slow-begin'); await new Promise(resolve => setTimeout(resolve, 1500)); log('slow-end'); });
  pi.on('session_start', () => { log('next-start'); });
  pi.on('session_shutdown', event => { log(`shutdown ${event.reason}`); });
};

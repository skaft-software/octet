import { performance } from 'node:perf_hooks';

// Off-screen startup attribution for the Node side of the Pi bridge. The host
// enables it with OCTET_STARTUP_TRACE (the same switch the Octet frontend uses)
// and forwards `octet-startup:` stderr lines verbatim, so one stream carries
// the host and adapter phases together. `elapsed` is microseconds since this
// Node process started: the only origin both sides can agree on. Nothing is
// written when tracing is off, and the channel never renders.
const enabled = ![undefined, '', '0'].includes(process.env.OCTET_STARTUP_TRACE);

export const traceEnabled = enabled;

export function tracePhase(phase, suffix = '') {
  if (!enabled) return;
  process.stderr.write(`octet-startup: ${phase} elapsed=${Math.round(performance.now() * 1000)}us${suffix ? ` ${suffix}` : ''}\n`);
}

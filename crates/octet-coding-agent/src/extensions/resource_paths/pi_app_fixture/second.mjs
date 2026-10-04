import { log, paths, state } from './common.mjs';

export default pi => {
  log('second', 'load');
  pi.on('session_start', (_event, ctx) => { log('second', 'started', { cwd: ctx.cwd }); });
  pi.on('resources_discover', (event, ctx) => {
    const current = state(event.cwd);
    log('second', 'discover', { cwd: event.cwd, contextCwd: ctx.cwd, reason: event.reason, phase: current.phase });
    return paths('second', current.phase);
  });
};

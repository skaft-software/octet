import { log, paths, state } from './common.mjs';

export default pi => {
  log('first', 'load');
  pi.on('session_start', async (_event, ctx) => {
    const current = state(ctx.cwd);
    if (current.failStart) {
      log('first', 'start_failed', { cwd: ctx.cwd });
      throw new Error('ordinary Pi factory refused session_start');
    }
    await Promise.resolve();
    log('first', 'started', { cwd: ctx.cwd });
  });
  pi.on('resources_discover', async (event, ctx) => {
    const current = state(event.cwd);
    await Promise.resolve();
    log('first', 'discover', { cwd: event.cwd, contextCwd: ctx.cwd, reason: event.reason, phase: current.phase });
    return paths('first', current.phase);
  });
};

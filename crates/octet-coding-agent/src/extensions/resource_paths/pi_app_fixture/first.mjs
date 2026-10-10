import { log, paths, state } from './common.mjs';

export default pi => {
  log('first', 'load');
  pi.on('session_start', async (_event, ctx) => {
    const current = state(ctx.cwd);
    if (current.startBehavior === 'throw') {
      log('first', 'start_failed', { cwd: ctx.cwd });
      throw new Error('private start failure details');
    }
    if (current.startBehavior === 'refused_write') {
      // Exercise a tracked mutation's actual typed headless-host refusal, not
      // a fabricated RPC error or an ordinary observer exception.
      log('first', 'start_refusal_requested', { cwd: ctx.cwd });
      try {
        await pi.setSessionName('refused-start-name');
      } catch (error) {
        log('first', 'start_refused', { code: error.code, message: error.message });
        throw error;
      }
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

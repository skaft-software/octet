// Unchanged Pi-style callback API, exercised through the real Runtime loader.
export default pi => {
  let retained;
  pi.registerCommand('compact_test', {
    async handler(mode, ctx) {
      if (mode === 'capture') { retained = ctx; pi.events.emit('compact:captured', ctx); return; }
      if (mode === 'retained') ctx = retained;
      const options = {
        customInstructions: 'Keep the actual decisions.\n\tDo not invent metrics.',
        onComplete: async result => {
          pi.events.emit('compact:complete', result);
          if (mode === 'throw-complete') throw new Error('completion callback threw');
          if (mode === 'callback-work') {
            pi.setSessionName('from callback');
            ctx.ui.setEditorText('callback draft');
            await new Promise(resolve => pi.events.emit('compact:callback-barrier', { resolve }));
          }
          ctx.ui.notify('compact completed');
        },
        onError: error => {
          pi.events.emit('compact:error', error);
          if (mode === 'throw-error') throw new Error('error callback threw');
          // Cancellation/retirement callbacks do not retain a foreground UI lease.
          if (!ctx.signal.aborted) ctx.ui.notify('compact failed');
        },
      };
      const result = ctx.compact(mode === 'default' ? undefined : options);
      pi.events.emit('compact:return', { result, ctx });
      if (mode === 'hold') await new Promise(resolve => pi.events.emit('compact:barrier', { resolve }));
      if (mode === 'fail') throw new Error('origin failed');
    },
  });
};

// Pi packages/coding-agent/src/core/extensions/runner.ts:1525-1574:
// handled stops the chain before persistence or provider invocation.
export default pi => {
  pi.on('input', (e, ctx) => {
    if (e.text === 'CONFORMANCE_HANDLE') {
      ctx.ui.notify('CONFORMANCE_HANDLED');
      return { action: 'handled' };
    }
    return { action: 'transform', text: e.text + ':TRANSFORMED' };
  });
};

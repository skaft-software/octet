export default pi => {
  pi.on('before_provider_request', async (event, ctx) => {
    if (ctx.sessionManager.getSessionName() === 'wait') await ctx.ui.confirm('pipeline barrier');
    if (event.payload.secret) throw new Error('PRIVATE:' + event.payload.secret);
    return { ...event.payload, max_output_tokens: 256, sequence: ['first'] };
  });
  pi.on('before_provider_request', event => { event.payload.sequence?.push('second'); });
  pi.on('before_provider_headers', event => {
    delete event.headers['x-remove']; event.headers['x-null'] = null;
    event.headers['x-added'] = ['first', 'second'];
  });
  pi.on('after_provider_response', (event, ctx) => { ctx.ui.notify(`actual-arrival:${event.status}`); });
}

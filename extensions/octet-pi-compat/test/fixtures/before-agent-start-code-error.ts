export default pi => {
  // A Node-style failure (string code), e.g. a locked SQLite memory database.
  pi.on('before_agent_start', () => { throw Object.assign(new Error('database is locked'), { code: 'SQLITE_BUSY' }); });
  pi.on('before_agent_start', event => ({ systemPrompt: `${event.systemPrompt}\nlater handler ran` }));
}

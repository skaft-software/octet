// Handlers that never settle, like a factory stuck on a network call, a child
// process or an MCP connection that never answers.
export default pi => {
  pi.on('session_start', () => new Promise(() => {}));
  pi.on('before_agent_start', () => new Promise(() => {}));
  pi.on('tool_call', () => new Promise(() => {}));
};

// A healthy neighbour: it must keep working while another factory stalls.
export default pi => {
  pi.on('session_start', async () => {});
  pi.on('before_agent_start', event => ({ systemPrompt: `${event.systemPrompt}\nhealthy` }));
};

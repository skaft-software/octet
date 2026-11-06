import { Agent } from '@earendil-works/pi-agent-core';
export default function (pi: any) {
  pi.registerCommand('agent-core', { description: `agent=${typeof Agent}`, handler: async () => {} });
}

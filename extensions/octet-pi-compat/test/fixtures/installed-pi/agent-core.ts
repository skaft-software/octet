// pi-agent-core has no octet shim; Pi 1.0.2 resolves it for every extension.
import { Agent } from '@earendil-works/pi-agent-core';
export default function (pi: any) {
  pi.registerCommand('agent-core', { description: `agent=${typeof Agent}`, handler: async () => {} });
}

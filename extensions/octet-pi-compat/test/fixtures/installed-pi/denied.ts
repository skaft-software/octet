// Real-only Pi names that would take over Octet-owned side effects are refused.
import { ProjectTrustStore } from '@earendil-works/pi-coding-agent';
import { createProvider } from '@earendil-works/pi-ai';
const refusal = (fn: () => unknown) => { try { fn(); return 'allowed'; } catch (error: any) { return String(error.message); } };
export default function (pi: any) {
  pi.registerCommand('trust', { description: refusal(() => new ProjectTrustStore()), handler: async () => {} });
  pi.registerCommand('provider', { description: refusal(() => createProvider({})), handler: async () => {} });
}

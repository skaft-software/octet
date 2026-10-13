// Imports a Pi export the Octet shims lack (parseSkillBlock) next to one they
// provide (VERSION, which must keep coming from the shim in every mode).
import { parseSkillBlock, VERSION } from '@earendil-works/pi-coding-agent';
const parsed = parseSkillBlock('plain text');
export default function (pi: any) {
  pi.registerCommand('installed-probe', { description: `parsed=${JSON.stringify(parsed)} version=${VERSION}`, handler: async () => {} });
}

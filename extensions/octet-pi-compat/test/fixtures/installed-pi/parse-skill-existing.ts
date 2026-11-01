import { parseSkillBlock, VERSION } from '@earendil-works/pi-coding-agent';
const parsed = parseSkillBlock('plain text');
export default function (pi: any) {
  pi.registerCommand('installed-probe', { description: `parsed=${JSON.stringify(parsed)} version=${VERSION}`, handler: async () => {} });
}

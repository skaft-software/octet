// A name present in installed Pi but not classified by this octet build.
import { fixtureOnlyHelper } from '@earendil-works/pi-coding-agent';
export default function (pi: any) {
  let description;
  try { fixtureOnlyHelper(); description = 'allowed'; } catch (error: any) { description = String(error.message); }
  pi.registerCommand('unclassified', { description, handler: async () => {} });
}

// Pi 1.4 extensions import model helpers from the pi-ai/models subpath.
import { clampThinkingLevel } from '@earendil-works/pi-ai/models';
export default function (pi: any) {
  const helper = clampThinkingLevel.name; // the shims lack it: path A must classify, not load
  pi.registerCommand('models-subpath', { description: `clamp=${helper}`, handler: async () => {} });
}

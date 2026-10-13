// Custom providers import pi-ai's provider building blocks from the compat subpath.
import { createAssistantMessageEventStream } from '@earendil-works/pi-ai/compat';
export default function (pi: any) {
  pi.registerCommand('compat-subpath', { description: `stream=${typeof createAssistantMessageEventStream}`, handler: async () => {} });
}

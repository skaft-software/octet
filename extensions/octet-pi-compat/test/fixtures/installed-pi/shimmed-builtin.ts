// The shim exports createBashTool but refuses it at load; path A must still offer the fallback.
import { createBashTool } from '@earendil-works/pi-coding-agent';
export default function (pi: any) {
  const bash = createBashTool(process.cwd());
  pi.registerCommand('shimmed-builtin', { description: `bash=${bash.name}`, handler: async () => {} });
}

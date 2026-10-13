// Pi built-in tool factories: refused in path A, real Pi in the approved installed-Pi fallback.
import { createBashTool, createReadToolDefinition } from '@earendil-works/pi-coding-agent';
export default function (pi: any) {
  const bash = createBashTool(process.cwd());
  const read = createReadToolDefinition(process.cwd());
  pi.registerCommand('builtin-tools', { description: `bash=${bash.name}:${typeof bash.execute} read=${read.name}:${typeof read.execute}`, handler: async () => {} });
}

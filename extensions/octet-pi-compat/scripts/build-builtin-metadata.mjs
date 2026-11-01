import { pathToFileURL } from 'node:url';
import { resolve, join } from 'node:path';

if (!process.argv[2]) throw new Error('supply the reviewed Pi node_modules/@earendil-works directory');
const tools = await import(pathToFileURL(join(resolve(process.argv[2]), 'pi-coding-agent/dist/core/tools/index.js')).href);
const definitions = {};
for (const name of ['Read', 'Edit', 'Write', 'Bash', 'PowerShell', 'Grep', 'Find', 'Ls']) {
  const definition = tools[`create${name}ToolDefinition`]('.');
  definitions[definition.name] = Object.fromEntries(Object.entries(definition).filter(([, value]) => typeof value !== 'function'));
}
process.stdout.write(JSON.stringify(definitions, null, 2) + '\n');

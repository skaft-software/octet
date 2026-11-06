// Loads the fixture's Pi setup through Pi 1.4.0's own resource loader (the
// same DefaultResourceLoader the CLI uses) and prints what loaded as JSON.
// Untimed: it supplies the Pi side of the parity metrics (which packages
// load, which commands they register). Usage:
//   node pi_probe.mjs <releaseDir> <agentDir> <cwd>
// HOME must already point at the fixture home.
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [releaseDir, agentDir, cwd] = process.argv.slice(2);
if (!releaseDir || !agentDir || !cwd) {
  console.error('usage: pi_probe.mjs <releaseDir> <agentDir> <cwd>');
  process.exit(2);
}
const entry = join(releaseDir, 'node_modules', '@earendil-works', 'pi-coding-agent', 'dist', 'index.js');
const { DefaultResourceLoader, SettingsManager } = await import(pathToFileURL(entry).href);
const settingsManager = SettingsManager.create(cwd, agentDir);
const loader = new DefaultResourceLoader({
  cwd, agentDir, settingsManager,
  noSkills: true, noThemes: true, noPromptTemplates: true, noContextFiles: true,
});
await loader.reload();
const result = loader.getExtensions();
const keys = value => (value && typeof value.keys === 'function' ? [...value.keys()] : []);
const out = {
  extensions: result.extensions.map(ext => ({
    path: ext.path,
    resolvedPath: ext.resolvedPath,
    source: ext.sourceInfo?.source,
    origin: ext.sourceInfo?.origin,
    baseDir: ext.sourceInfo?.baseDir,
    commands: keys(ext.commands),
    tools: keys(ext.tools),
    events: keys(ext.handlers),
    flags: keys(ext.flags),
    shortcuts: keys(ext.shortcuts),
  })),
  errors: result.errors ?? [],
  warnings: result.warnings ?? [],
};
process.stdout.write(`PI-PROBE-JSON ${JSON.stringify(out)}\n`);
// Extensions can leave timers and sockets open; the probe is done.
process.exit(0);

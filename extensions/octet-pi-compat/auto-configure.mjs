// Invoked only by the native activation path, after enablement, source trust,
// integrity and process-policy admission. Enabling the shipped adapter opts in
// to the live Pi setup; explicit configure.mjs snapshots keep their old contract.
import { configureMirror } from './configure.mjs';

const [output, projectTrusted] = process.argv.slice(2);
if (!output || !['true', 'false'].includes(projectTrusted)) throw new Error('native mirror activation arguments required');
await configureMirror({ output, reviewed: true, projectTrusted: projectTrusted === 'true', cache: true,
  log: message => console.error(`[pi-compat] ${message}`) });

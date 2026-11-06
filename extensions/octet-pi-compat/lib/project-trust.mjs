// Pi 1.0.2 trust-manager.js's no-project-resources rule (MIT; LICENSE.pi).
import { lstatSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

const projectResources = ['settings.json', 'mcp.json', 'extensions', 'skills', 'prompts', 'themes', 'SYSTEM.md', 'APPEND_SYSTEM.md'];

function present(path) {
  try { lstatSync(path); return true; }
  catch (error) {
    return !['ENOENT', 'ENOTDIR'].includes(error.code);
  }
}

export function retainPiProjectTrust(state) {
  if (state.host.project_trusted === true) state.piPlainWorkspaceTrusted = false;
  else if (state.piPlainWorkspaceTrusted !== false) {
    state.piPlainWorkspaceTrusted = piProjectTrusted({ cwd: state.workspace, hostTrusted: false });
  }
}

export function piProjectTrusted({ cwd, hostTrusted, home = process.env.HOME || homedir() }) {
  if (hostTrusted) return true;
  let current, userSkills;
  try {
    current = realpathSync(cwd);
    userSkills = join(realpathSync(home), '.agents', 'skills');
  } catch { return false; }
  if (projectResources.some(entry => present(join(current, '.pi', entry)))) return false;
  for (;;) {
    const skills = join(current, '.agents', 'skills');
    if (skills !== userSkills && present(skills)) return false;
    const parent = dirname(current);
    if (parent === current) return true;
    current = parent;
  }
}

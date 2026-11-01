// Pi 1.0.2 trust-manager.js's no-project-resources rule (MIT; LICENSE.pi).
// This is only the signal exposed to already admitted Pi factories. It never
// grants native workspace trust or admits project resources into the mirror.
import { lstatSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

const projectResources = ['settings.json', 'mcp.json', 'extensions', 'skills', 'prompts', 'themes', 'SYSTEM.md', 'APPEND_SYSTEM.md'];

function present(path) {
  try { lstatSync(path); return true; }
  catch (error) {
    // An unreadable or linked/dangling resource must not turn into a trust grant.
    return !['ENOENT', 'ENOTDIR'].includes(error.code);
  }
}

// Keep the plain-workspace exception within a single owner. Once resources
// appear or native trust is granted, only the host can subsequently grant trust;
// removing resources or revoking host trust must not silently promote an owner.
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

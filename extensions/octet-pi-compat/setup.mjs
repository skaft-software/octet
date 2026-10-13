#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('.', import.meta.url));
// Deliberate local opt-in; never called by discovery, installation or the runner.
// No lifecycle scripts, global installs, Pi runtime, assets or model downloads.
const result = spawnSync(process.platform === 'win32' ? 'npm.cmd' : 'npm',
  ['ci', '--ignore-scripts', '--no-audit', '--no-fund', '--cache', `${root}/.npm-cache`],
  { cwd: root, stdio: 'inherit' });
if (result.error) { console.error(result.error.message); process.exitCode = 1; }
else process.exitCode = result.status ?? 1;

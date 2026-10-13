'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const PLATFORMS = Object.freeze({
  'darwin-arm64': '@skaft/octet-darwin-arm64',
  'darwin-x64': '@skaft/octet-darwin-x64',
  'linux-x64': '@skaft/octet-linux-x64-gnu',
  'win32-x64': '@skaft/octet-win32-x64',
});

function selectedPackage(platform = process.platform, architecture = process.arch) {
  if (platform === 'linux' && architecture === 'x64') {
    const libc = process.report?.getReport?.().header?.glibcVersionRuntime;
    if (!libc) throw new Error('octet npm package requires GNU libc on Linux; musl is not supported');
  }
  const key = `${platform}-${architecture}`;
  const selected = PLATFORMS[key];
  if (!selected) throw new Error(`octet npm package does not support this platform: ${key}`);
  return selected;
}

function findPlatformRoot(launcherRoot, packageName) {
  const packageLeaf = packageName.slice(packageName.indexOf('/') + 1);
  const candidates = [
    path.join(launcherRoot, 'node_modules', ...packageName.split('/')),
    path.join(launcherRoot, '..', packageLeaf),
  ];
  for (const candidate of candidates) {
    try {
      if (fs.lstatSync(candidate).isDirectory() && !fs.lstatSync(candidate).isSymbolicLink()) return candidate;
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }
  throw new Error(`octet npm package is missing the selected platform runtime: ${packageName}`);
}

function launch(commandName, args = process.argv.slice(2)) {
  if (commandName !== 'octet' && commandName !== 'octet-host') throw new Error('invalid native command');
  const launcherRoot = path.resolve(__dirname, '..');
  const packageName = selectedPackage();
  const platformRoot = findPlatformRoot(launcherRoot, packageName);
  const executableName = process.platform === 'win32' ? `${commandName}.exe` : commandName;
  const executable = path.join(platformRoot, 'bin', executableName);
  const versionPath = path.join(platformRoot, 'share', 'octet', '.octet-version');
  const required = [
    'package.json', 'README.md', 'LICENSE', executable,
    path.join(platformRoot, 'share', 'octet', 'README.md'),
    path.join(platformRoot, 'share', 'octet', 'docs'),
    path.join(platformRoot, 'share', 'octet', 'examples'),
    path.join(platformRoot, 'share', 'octet', 'sdk'),
  ];
  for (const file of required) {
    try {
      const stat = fs.lstatSync(file);
      if (stat.isSymbolicLink() || (file === executable ? !stat.isFile() : !stat.isFile() && !stat.isDirectory())) {
        throw new Error(`octet npm platform package is incomplete: ${platformRoot}`);
      }
    } catch (error) {
      if (error.code === 'ENOENT') throw new Error(`octet npm platform package is incomplete: ${platformRoot}`);
      throw error;
    }
  }
  if (process.platform !== 'win32' && (fs.statSync(executable).mode & 0o111) === 0) {
    throw new Error(`octet npm platform executable is not executable: ${executable}`);
  }
  const version = fs.readFileSync(path.join(launcherRoot, 'package.json'), 'utf8').match(/"version"\s*:\s*"([^"]+)"/);
  if (!version || fs.readFileSync(versionPath, 'utf8').trim() !== version[1]) {
    throw new Error('octet npm platform version does not match launcher version');
  }
  const result = spawnSync(executable, args, { cwd: process.cwd(), env: process.env, stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.signal) {
    process.kill(process.pid, result.signal);
    return;
  }
  process.exitCode = result.status === null ? 1 : result.status;
}

module.exports = { PLATFORMS, selectedPackage, findPlatformRoot, launch };

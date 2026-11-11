// Pi 1.5.0, 9c9ce8547e5a88cbe834a53e9a6d83a789d08e47:
// packages/coding-agent/src/core/footer-data-provider.ts:16-49,239-265.
// Ancestor/worktree lookup, detached HEAD and the reftable sentinel match Pi.
// Unlike Pi's first synchronous read, this process boundary prepares the cache
// before mounting: a component's getGitBranch() never does filesystem/host I/O.
import { constants } from 'node:fs';
import { lstat, open } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { facade, invalid } from './errors.mjs';
import { nativeTimer } from './timers.mjs';

async function readMetadata(path) {
  const file = await open(path, constants.O_RDONLY | constants.O_NONBLOCK);
  try {
    const stat = await file.stat();
    if (!stat.isFile() || stat.size > 4096) return null;
    const bytes = Buffer.alloc(4097), { bytesRead } = await file.read(bytes, 0, bytes.length, 0);
    return bytesRead > 4096 ? null : new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, bytesRead)).trim();
  } finally { await file.close(); }
}
async function findHead(cwd) {
  for (let directory = resolve(cwd); ; directory = dirname(directory)) {
    const git = join(directory, '.git');
    try {
      const stat = await lstat(git);
      if (stat.isDirectory()) return { repo: directory, head: join(git, 'HEAD') };
      if (stat.isFile()) {
        const text = await readMetadata(git);
        if (text?.startsWith('gitdir: ')) return { repo: directory, head: join(resolve(directory, text.slice(8).trim()), 'HEAD') };
      }
      return null;
    } catch (error) { if (error.code !== 'ENOENT') return null; }
    if (dirname(directory) === directory) return null;
  }
}
const validBranch = branch => branch && !/[\x00-\x20\x7f-\x9f]/u.test(branch) ? branch : null;
const reftableBranch = repo => new Promise(resolve => {
  execFile('git', ['--no-optional-locks', 'symbolic-ref', '--quiet', '--short', 'HEAD'],
    { cwd: repo, encoding: 'utf8', timeout: 1000, maxBuffer: 4096, windowsHide: true },
    (error, stdout) => resolve(error ? null : validBranch(stdout.trim())));
});
async function readBranch(paths) {
  if (!paths) return null;
  try {
    const head = await readMetadata(paths.head);
    if (head === null) return null;
    if (!head.startsWith('ref: refs/heads/')) return 'detached';
    const branch = validBranch(head.slice(16));
    return branch === '.invalid' ? await reftableBranch(paths.repo) ?? 'detached' : branch;
  } catch { return null; }
}

export function createFooterData(runtime, store, tui) {
  const state = store.state;
  let closed = false, cwd, paths, branch = null, pending;
  const live = () => !closed && state.alive && !store.surface?.closed;
  const authoritative = () => Object.hasOwn(state.host, 'git_branch');
  const refresh = () => {
    if (!live() || pending || authoritative()) return pending;
    pending = (async () => {
      const workspace = state.workspace;
      if (cwd !== workspace) { paths = await findHead(workspace); cwd = workspace; }
      const next = await readBranch(paths);
      if (!live() || state.workspace !== workspace || authoritative()) return;
      const previous = branch; branch = next;
      if (previous !== next && initialized) {
        for (const listener of state.branchListeners) listener();
        tui.requestRender();
      }
    })().finally(() => { pending = undefined; });
    // An unreadable repository is no branch, not an extension failure. There
    // is at most one asynchronous lookup, even on a stalled filesystem.
    pending.catch(() => {});
    return pending;
  };
  let initialized = false;
  const timer = nativeTimer(250);
  const ready = Promise.race([refresh(), timer.promise]).finally(() => { timer.cancel(); initialized = true; });
  const interval = setInterval(refresh, 500);
  interval.unref?.();
  const listeners = new Set();
  return {
    ready,
    data: facade({
      getGitBranch() { runtime.assertOwner(store); return authoritative() ? state.host.git_branch : branch; },
      getExtensionStatuses() { runtime.assertOwner(store); return new Map(state.statuses); },
      onBranchChange(handler) {
        runtime.assertOwner(store);
        if (typeof handler !== 'function') invalid('branch change listener');
        if (!live()) return () => {};
        const bound = () => {
          if (!live()) return;
          try { runtime.scope.run(store, handler); }
          catch (error) { runtime.reportCallbackError('footer branch change', store.factory, error); }
        };
        listeners.add(bound); state.branchListeners.add(bound);
        return () => { listeners.delete(bound); state.branchListeners.delete(bound); };
      },
    }, 'footerData'),
    dispose() {
      closed = true; clearInterval(interval);
      for (const listener of listeners) state.branchListeners.delete(listener);
      listeners.clear();
    },
  };
}

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch, owner } from './helper.mjs';

const plain = frame => frame.params.lines.join('\n').replace(/\x1b\[[0-9;]*m/g, '');
const branchFrame = frame => frame.method === 'ui/frame' && plain(frame).startsWith('branch:');
async function fixture(t, { head = 'ref: refs/heads/main\n', nested = false, worktree = false, supplied, throwing = false } = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'octet-footer-branch-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const repo = join(directory, 'repo'), workspace = nested ? join(repo, 'src/nested') : repo;
  await mkdir(workspace, { recursive: true });
  const git = worktree ? join(directory, 'git/worktrees/example') : join(repo, '.git');
  await mkdir(git, { recursive: true });
  if (worktree) await writeFile(join(repo, '.git'), 'gitdir: ../git/worktrees/example\n');
  await writeFile(join(git, 'HEAD'), head);
  const factory = join(directory, 'footer.mjs');
  await writeFile(factory, `export default pi => { pi.on('session_start', (_, ctx) => ctx.ui.setFooter((tui, theme, data) => {
    let changes = 0;
    ${throwing ? "data.onBranchChange(() => { throw new Error('private branch exception'); });" : ''}
    const off = data.onBranchChange(() => { changes++; tui.requestRender(); });
    return { render: () => ['branch:' + String(data.getGitBranch()) + ' changes:' + changes], invalidate() {}, dispose: off };
  })); };`);
  const peer = launch(t, [factory], { cwd: workspace }); await peer.init();
  await peer.start(supplied === undefined ? {} : { git_branch: supplied });
  return { peer, git };
}

test('footer branch lookup finds ancestor repositories and reports detached HEAD like Pi', async t => {
  for (const worktree of [false, true]) {
    const { peer } = await fixture(t, { nested: true, worktree, head: 'a'.repeat(40) + '\n' });
    const frame = await peer.wait(branchFrame);
    assert.match(plain(frame), /^branch:detached changes:0$/);
    await peer.close();
  }
});

test('footer branch data refreshes after HEAD replacement without a host update', async t => {
  const { peer, git } = await fixture(t);
  assert.match(plain(await peer.wait(branchFrame)), /^branch:main changes:0$/);
  await writeFile(join(git, 'HEAD'), 'ref: refs/heads/next\n');
  const changed = await peer.wait(frame => branchFrame(frame) && plain(frame) === 'branch:next changes:1');
  assert.equal(changed.params.lines.length, 1);
  await peer.close();
});

test('an explicit native null branch is authoritative, not a request for filesystem fallback', async t => {
  const { peer } = await fixture(t, { supplied: null });
  assert.equal(plain(await peer.wait(branchFrame)), 'branch:null changes:0');
  peer.notify('context/updated', { resource_owner: owner, host: { git_branch: 'host-branch' } });
  assert.match(plain(await peer.wait(frame => branchFrame(frame) && plain(frame).startsWith('branch:host-branch'))), /changes:1$/);
  await peer.close();
});

test('branch listener failures are isolated and sanitized by footer stage', async t => {
  const { peer, git } = await fixture(t, { throwing: true });
  await peer.wait(branchFrame);
  await writeFile(join(git, 'HEAD'), 'ref: refs/heads/survives\n');
  await peer.wait(frame => branchFrame(frame) && plain(frame) === 'branch:survives changes:1');
  const warning = await peer.wait(frame => frame.method === 'notification' && frame.params.title === '[Extension issues]');
  assert.match(warning.params.message, /Footer.*footer.*branch/i);
  assert.doesNotMatch(JSON.stringify(warning), /private branch exception/);
  await peer.close();
});


test('footer branch data is null outside a repository and subscriptions end with their surface', async t => {
  const { peer, git } = await fixture(t);
  await peer.wait(branchFrame);
  const open = peer.seen.find(frame => frame.method === 'ui/open');
  await rm(git, { recursive: true });
  await peer.wait(frame => branchFrame(frame) && plain(frame) === 'branch:null changes:1');
  peer.notify('ui/closed', { surface_id: open.params.surface_id, reason: 'rescue' });
  await peer.close();
  assert.doesNotMatch(peer.stderr(), /footer branch change hook/);
});

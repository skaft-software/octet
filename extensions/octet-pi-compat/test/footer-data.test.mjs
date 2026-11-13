import test from 'node:test';
import assert from 'node:assert/strict';
import { AsyncLocalStorage } from 'node:async_hooks';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createFooterData } from '../lib/footer-data.mjs';

async function harness(t) {
  const workspace = await mkdtemp(join(tmpdir(), 'octet-footer-data-'));
  t.after(() => rm(workspace, { recursive: true, force: true }));
  let refresh, renders = 0;
  t.mock.method(globalThis, 'setInterval', callback => {
    refresh = callback;
    return { unref() {} };
  });
  t.mock.method(globalThis, 'clearInterval', () => {});
  const state = { workspace, alive: true, host: {}, statuses: new Map(), branchListeners: new Set() };
  const store = { state, factory: 'footer-fixture', surface: { closed: false } };
  const errors = [];
  const runtime = {
    scope: new AsyncLocalStorage(),
    assertOwner(value) { assert.equal(value, store); assert.ok(state.alive); },
    reportCallbackError(...args) { errors.push(args); },
  };
  const footer = createFooterData(runtime, store, { requestRender() { renders++; } });
  t.after(() => footer.dispose());
  await footer.ready;
  return { workspace, state, store, runtime, footer, errors, refresh: () => refresh(), renders: () => renders };
}

test('footer discovery retries after a repository is created in the same workspace', async t => {
  const h = await harness(t);
  assert.equal(h.footer.data.getGitBranch(), null);
  let changes = 0;
  h.footer.data.onBranchChange(() => { changes++; });
  await mkdir(join(h.workspace, '.git'));
  await writeFile(join(h.workspace, '.git/HEAD'), 'ref: refs/heads/new-repo\n');
  // Painting only reads the cache; discovery still belongs to the refresh trigger.
  assert.equal(h.footer.data.getGitBranch(), null);
  await h.refresh();
  assert.equal(h.footer.data.getGitBranch(), 'new-repo');
  assert.equal(changes, 1);
  assert.equal(h.renders(), 1);
});

test('async footer branch listener rejections are isolated like synchronous throws', async t => {
  const h = await harness(t);
  let changes = 0;
  h.footer.data.onBranchChange(async () => {
    assert.equal(h.runtime.scope.getStore(), h.store);
    await Promise.resolve();
    throw new Error('private branch exception');
  });
  h.footer.data.onBranchChange(() => { changes++; });
  for (const listener of h.state.branchListeners) listener();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(changes, 1, 'a rejected listener does not stop later listeners');
  assert.equal(h.errors.length, 1, 'the rejection reaches the existing sanitized reporter');
  assert.deepEqual(h.errors[0].slice(0, 2), ['footer branch change', 'footer-fixture']);
});

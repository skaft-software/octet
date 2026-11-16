import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CHROME_SCHEMA, ChromeSnapshot, chromeSlot, editorTemplate, footerTemplate, safeChromeLiteral } from '../lib/chrome-snapshot.mjs';

test('chrome templates persist presentation without drafts, session text or numeric usage', () => {
  const lines = ['\x1b[35m────\x1b[39m', 'private draft', '────'];
  assert.deepEqual(editorTemplate(lines, 4), {top: lines[0], bottom: lines[2], padding: 0, text_width: 3, max_rows: 7});
  assert.equal(editorTemplate(['private title', 'draft', '────'], 12), undefined);
  const footer = footerTemplate(['\x1b[35mmock-1 · 435 context · $2.73\x1b[0m'], {model: 'mock-1'});
  const text = JSON.stringify(footer);
  assert.ok(!text.includes('435') && !text.includes('2.73') && !text.includes('mock-1'));
  assert.ok(footer[0].some(part => part.field === 'model'));
  for (const unsafe of ['session-title', 'sk-secret', 'notification', '\x1b]0;title\x07']) {
    assert.throws(() => footerTemplate([unsafe], {}));
    assert.equal(safeChromeLiteral(unsafe), false);
  }
});

test('snapshot identity includes settings, versions, reviewed sources and directory membership', () => {
  const root = mkdtempSync(join(tmpdir(), 'chrome-test-'));
  process.env.OCTET_PI_COMPAT_CACHE = join(root, 'cache');
  const agent = join(root, 'agent'), workspace = join(root, 'work');
  mkdirSync(agent); mkdirSync(workspace); mkdirSync(join(agent, 'extensions'));
  const source = join(agent, 'extensions', 'chrome.mjs');
  writeFileSync(source, 'export default () => {}');
  writeFileSync(join(agent, 'settings.json'), '{"theme":"dark"}');
  const config = {mirror_pi_setup: true, pi_agent_dir: agent, extensions: [source]};
  const cache = new ChromeSnapshot({config});
  cache.prepare(workspace, false);
  const surface = {nativeEditorObjects: new Set([{startupRows: 1, startupPadding: 0}]), placement: 'editor', columns: 80, rows: 24, store: {state: {workspace, host: {project_trusted: false}}}};
  cache.capture(surface, ['─'.repeat(80), 'NEVER-PERSIST-THIS-DRAFT', '─'.repeat(80)]);
  surface.placement = 'footer';
  surface.store.state.host.model_view = {id: 'mock-1'};
  cache.capture(surface, ['mock-1 · 874 tokens']);
  const file = join(root, 'cache', chromeSlot(agent, workspace, false));
  const text = readFileSync(file, 'utf8'), snapshot = JSON.parse(text);
  assert.equal(snapshot.schema, CHROME_SCHEMA);
  assert.ok(snapshot.editor && snapshot.footer);
  assert.ok(!text.includes('NEVER-PERSIST') && !text.includes('874'));
  assert.ok(snapshot.identity.some(item => item.path === join(agent, 'settings.json') && item.hash));
  assert.ok(snapshot.identity.some(item => item.path === join(agent, 'npm/package-lock.json') && item.hash === null));
  assert.ok(snapshot.identity.some(item => item.directory && item.path === join(agent, 'extensions')));
  assert.ok(snapshot.identity.some(item => item.path === source && item.hash));
  assert.equal(readdirSync(join(root, 'cache')).filter(name => name.startsWith('chrome-')).length, 1);
  writeFileSync(join(agent, 'settings.json'), '{"theme":"light"}');
  surface.store.state.host.model_view = {id: 'changed-model'};
  cache.capture(surface, ['changed-model · 9 tokens']);
  assert.equal(readFileSync(file, 'utf8'), text, 'a settings edit during loading cannot bless old chrome');
  delete process.env.OCTET_PI_COMPAT_CACHE;
});

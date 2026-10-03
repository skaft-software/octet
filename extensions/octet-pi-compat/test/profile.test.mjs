import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

const version = async name => JSON.parse(await readFile(new URL(`../node_modules/${name}/package.json`, import.meta.url))).version;

test('Pi0.85 selected TUI and genuine TypeBox1 imports remain separate from legacy TypeBox', async t => {
  assert.equal(await version('@earendil-works/pi-tui'), '0.85.0');
  assert.equal(await version('typebox'), '1.1.12');
  assert.equal(await version('@sinclair/typebox'), '0.34.41');
  const dir = await mkdtemp(join(tmpdir(), 'octet-pi-profile-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'profile.ts');
  await writeFile(entry, `import { Type } from 'typebox';
import { Type as Legacy } from '@sinclair/typebox';
import { stripTerminalSequences, visibleWidth } from '@earendil-works/pi-tui';
import { StringEnum } from '@earendil-works/pi-ai';
export default pi => {
  if (Type === Legacy) throw new Error('incorrect TypeBox alias');
  if (stripTerminalSequences('\\x1b[31mred\\x1b[0m') !== 'red' || visibleWidth('red') !== 3) throw new Error('TUI utility import');
  pi.registerTool({ name: 'profile_test', label: 'Profile', description: 'Real schema imports',
    parameters: Type.Object({ text: Type.String(), mode: StringEnum(['a','b']), legacy: Legacy.Number() }),
    async execute() { return { content: [{ type: 'text', text: 'ok' }] }; }
  });
};`);
  const peer = launch(t, [entry]);
  await peer.init();
  const call = peer.request('tool/call', { name: 'profile_test', arguments: { text: 'test', mode: 'a', legacy: 1 }, context: peer.context() });
  const response = await call.response;
  assert.ok(response.result, JSON.stringify(response));
  assert.equal(response.result.content[0].text, 'ok');
  await peer.close();
});

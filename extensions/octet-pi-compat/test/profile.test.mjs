import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';
import { PI_VERSION } from '../lib/pi-modules.mjs';

const version = async name => JSON.parse(await readFile(new URL(`../node_modules/${name}/package.json`, import.meta.url))).version;

test('dependencies are the exact versions Pi 1.0.2 ships', async () => {
  assert.equal(PI_VERSION, '1.0.2');
  assert.equal(await version('@earendil-works/pi-tui'), '1.0.2');
  assert.equal(await version('jiti'), '2.7.0');
  assert.equal(await version('typebox'), '1.3.27');
  assert.equal(await version('yaml'), '2.9.0');
  assert.equal(await version('marked'), '18.0.11');
});

test('TypeBox and pi-ai resolve as in Pi 1.0.2, including subpaths', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'octet-pi-profile-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const entry = join(dir, 'profile.ts');
  await writeFile(entry, `import { Type } from 'typebox';
import { Value } from 'typebox/value';
import { Compile } from 'typebox/compile';
import * as System from 'typebox/system';
import { Type as Legacy } from '@sinclair/typebox';
import { Value as LegacyValue } from '@sinclair/typebox/value';
import { stripTerminalSequences, visibleWidth } from '@earendil-works/pi-tui';
import { StringEnum } from '@earendil-works/pi-ai';
import { StringEnum as CompatEnum } from '@earendil-works/pi-ai/compat';
export default pi => {
  // Pi 1.0.2 maps @sinclair/typebox onto its single TypeBox 1.x install.
  if (Type !== Legacy || Value !== LegacyValue) throw new Error('TypeBox spellings differ');
  if (typeof Compile !== 'function' || !Object.keys(System).length) throw new Error('TypeBox subpaths');
  if (StringEnum !== CompatEnum) throw new Error('pi-ai root and compat differ');
  if (stripTerminalSequences('\\x1b[31mred\\x1b[0m') !== 'red' || visibleWidth('red') !== 3) throw new Error('TUI utility import');
  const mode = StringEnum(['a', 'b'], { description: 'mode' });
  if (JSON.stringify(mode) !== JSON.stringify({ type: 'string', enum: ['a', 'b'], description: 'mode' })) throw new Error('StringEnum shape ' + JSON.stringify(mode));
  pi.registerTool({ name: 'profile_test', label: 'Profile', description: 'Real schema imports',
    parameters: Type.Object({ text: Type.String(), mode }),
    async execute(_id, args) { return { content: [{ type: 'text', text: Value.Check(Type.String(), args.text) ? 'ok' : 'bad' }] }; }
  });
};`);
  const peer = launch(t, [entry]);
  await peer.init();
  const call = peer.request('tool/call', { name: 'profile_test', arguments: { text: 'test', mode: 'a' }, context: peer.context() });
  const response = await call.response;
  assert.ok(response.result, JSON.stringify(response));
  assert.equal(response.result.content[0].text, 'ok');
  await peer.close();
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configure } from '../configure.mjs';
import { launch, root } from './helper.mjs';

async function factory(t, definitions) {
  const directory = await mkdtemp(join(tmpdir(), 'octet-prompt-metadata-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const entry = join(directory, 'factory.ts');
  await writeFile(entry, `export default pi => {
    for (const [i, metadata] of ${JSON.stringify(definitions)}.entries()) pi.registerTool({
      name: 'metadata_' + i, description: 'ordinary tool description', parameters: { type: 'object' },
      ...metadata, async execute() { return { content: [{ type: 'text', text: 'executed' }] }; }
    });
  };`);
  return { directory, entry };
}

test('tool prompt metadata maps verbatim to the negotiated tool catalog, not the manifest, and tools really execute', async t => {
  const definitions = [
    { promptSnippet: '  concise\nsummary\t ', promptGuidelines: ['  first  ', '', '\t', 'second\nline'] },
    { promptSnippet: '', promptGuidelines: [] },
    {},
    { promptSnippet: '😀'.repeat(256), promptGuidelines: Array(16).fill('é'.repeat(512)) },
  ];
  const { entry, directory } = await factory(t, definitions);
  const output = join(directory, 'octet-pi-compat');
  const { registrations } = configure({ reviewed: true, output, extensions: [entry] });
  const peer = launch(t, [entry], { config: join(output, 'bridge.json') });
  // Declare the reviewed hook surface, including configured palette discovery.
  peer.metadata.hooks = registrations.hooks;
  const result = await peer.init(['tool_prompt_metadata_v1', 'session_entries', 'pipeline_hooks_v1', 'resource_paths_v1']);
  assert.ok(result.protocol.features.includes('tool_prompt_metadata_v1'));
  assert.deepEqual(result.tools, peer.metadata.tools);
  for (const [i, expected] of definitions.entries()) {
    assert.equal(result.tools[i].prompt_snippet, expected.promptSnippet);
    assert.deepEqual(result.tools[i].prompt_guidelines, expected.promptGuidelines);
  }
  assert.deepEqual(Object.keys(result.tools[2]).sort(), ['description', 'name', 'nested_execution', 'parameters']);
  assert.doesNotMatch(await readFile(join(output, 'extension.toml'), 'utf8'), /prompt_snippet|prompt_guidelines|tool_prompt_metadata_v1/);
  const call = await peer.request('tool/call', { name: 'metadata_0', arguments: {}, context: peer.context() }).response;
  assert.equal(call.result.content[0].text, 'executed');
  assert.equal(peer.seen.some(f => f.method === 'ui/autocomplete/register'), false);
  await peer.close();
});

test('only tools that declare prompt metadata require the feature, including explicit empty values', async t => {
  for (const definition of [{}, { promptSnippet: '' }, { promptGuidelines: [] }]) {
    const { entry } = await factory(t, [definition]);
    const peer = launch(t, [entry]);
    if (Object.keys(definition).length) await assert.rejects(peer.init([]), /unsupported_feature tool_prompt_metadata_v1/);
    else assert.equal((await peer.init([])).tools.length, 1);
    await peer.close();
  }
});

test('prompt metadata rejects bad types, byte/count overflows, invalid Unicode and C0/C1 controls during actual factory load', async t => {
  const cases = [
    [{ promptSnippet: null }, /promptSnippet must be UTF-8 text/],
    [{ promptSnippet: '😀'.repeat(257) }, /bounds_exceeded tool promptSnippet/],
    [{ promptSnippet: '\ud800' }, /promptSnippet must be UTF-8 text/],
    [{ promptSnippet: '\r' }, /promptSnippet.*control/],
    [{ promptSnippet: '\x1b[31m' }, /promptSnippet.*control/],
    [{ promptGuidelines: null }, /promptGuidelines must contain at most 16 strings/],
    [{ promptGuidelines: Array(17).fill('x') }, /promptGuidelines must contain at most 16 strings/],
    [{ promptGuidelines: [7] }, /promptGuidelines entry must be UTF-8 text/],
    [{ promptGuidelines: ['é'.repeat(513)] }, /bounds_exceeded tool promptGuidelines entry/],
    [{ promptGuidelines: ['\u0085'] }, /promptGuidelines entry.*control/],
  ];
  for (const [definition, expected] of cases) {
    const { entry } = await factory(t, [definition]);
    const process = spawnSync(globalThis.process.execPath, [join(root, 'runner.mjs'), '--inspect', entry], { encoding: 'utf8', timeout: 10000 });
    assert.equal(process.status, 1); assert.match(process.stderr, expected);
    assert.equal(process.stdout, '', 'failed inspection must not publish a partial successful catalog');
  }
  const { entry } = await factory(t, [{}]);
  await writeFile(entry, (await readFile(entry, 'utf8')).replace('...metadata,', '...metadata, promptGuidelines: Array(1),'));
  const sparse = spawnSync(process.execPath, [join(root, 'runner.mjs'), '--inspect', entry], { encoding: 'utf8', timeout: 10000 });
  assert.equal(sparse.status, 1); assert.match(sparse.stderr, /promptGuidelines entry must be UTF-8 text/);
  assert.equal(sparse.stdout, '');
});

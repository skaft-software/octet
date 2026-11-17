// Synthetic package only: no npm subagent installation, worker or provider calls.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { launch } from './helper.mjs';

const nativeTools = ['subagent_spawn', 'subagent_status', 'subagent_wait', 'subagent_stop', 'subagent_continue', 'subagent_models'];
function fixture(t) {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), 'octet-pi-subagents-')));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  writeFileSync(join(directory, 'package.json'), JSON.stringify({ name: 'pi-subagents', version: '0.59.0', displayName: 'Custom fleet label', type: 'module' }));
  const entry = join(directory, 'index.mjs');
  writeFileSync(entry, `export default pi => {
    const tool = name => ({name,description:'Pi async runs',parameters:{type:'object',properties:{all:{type:'boolean'}}},
      execute(){return {content:[{type:'text',text:'Pi async runs'}]};}});
    for (const name of ${JSON.stringify(nativeTools.slice(0, -1))}) pi.registerTool(tool(name));
    pi.registerTool(tool('pi_only'));
    pi.registerCommand('check',{handler(){
      for (let i=0;i<2;i++) {
        pi.registerTool(tool('subagent_wait'));
        pi.registerTool(tool('subagent_models'));
      }
      pi.registerTool(tool('late_free'));
    }});
  };`);
  return entry;
}
const notices = peer => peer.seen.filter(frame => frame.method === 'notification' && frame.params.title === '[Extension issues]');

for (const enabled of [true, false]) test(`native subagent ownership ${enabled ? 'withholds collisions' : 'leaves Pi tools available'} and preserves package commands`, async t => {
  const entry = fixture(t);
  const peer = launch(t, [entry], { env: { OCTET_PI_NATIVE_SUBAGENT_TOOLS: JSON.stringify(enabled ? nativeTools : []) } });
  const initialized = await peer.init(['dynamic_tools']);
  // Review/admission uses the original definitions. The native host separately
  // filters its wire catalog; the adapter removes its conflicting local handlers.
  assert.ok(initialized.tools.some(tool => tool.name === 'subagent_wait'));
  const check = peer.command('check');
  if (!enabled) {
    for (const name of ['subagent_wait', 'subagent_models', 'subagent_wait', 'subagent_models']) {
      const registration = await peer.wait(frame => frame.method === 'tools/register');
      assert.equal(registration.params.tools[0].name, name);
      peer.send({ jsonrpc: '2.0', id: registration.id, result: { revision: 1, tools: [name] } });
    }
  }
  const registration = await peer.wait(frame => frame.method === 'tools/register');
  assert.equal(registration.params.tools[0].name, 'late_free', 'only unrelated registrations reach the host when native ownership is enabled');
  peer.send({ jsonrpc: '2.0', id: registration.id, result: { revision: 2, tools: ['late_free'] } });
  assert.ok((await check.response).result, peer.stderr());
  for (const name of nativeTools) {
    const reply = await peer.request('tool/call', { name, arguments: {}, context: peer.context() }).response;
    if (enabled) assert.match(reply.error.message, /unknown hosted tool/);
    else assert.equal(reply.result.content[0].text, 'Pi async runs');
  }
  for (const name of ['pi_only', 'late_free']) {
    const reply = await peer.request('tool/call', { name, arguments: {}, context: peer.context() }).response;
    assert.equal(reply.result.content[0].text, 'Pi async runs');
  }
  const shown = notices(peer);
  assert.equal(shown.length, enabled ? 2 : 0, 'one startup warning and one for the first late collision; replacements do not repeat either');
  if (enabled) {
    for (const notice of shown) {
      assert.equal(notice.params.level, 'warning');
      assert.match(notice.params.message, /^pi-subagents: .*octet-subagents owns/);
      assert.doesNotMatch(notice.params.message, /Custom fleet label/);
    }
    for (const name of nativeTools) assert.equal(shown.filter(notice => notice.params.message.includes(`"${name}"`)).length, 1, name);
  }
  await peer.close();
});

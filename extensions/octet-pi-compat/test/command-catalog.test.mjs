import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../lib/runtime.mjs';
import { createAPI } from '../lib/api.mjs';
import { discoverPiSetup } from '../lib/pi-setup.mjs';
import { owner } from './helper.mjs';
import { mkdtempSync, realpathSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

function harness() {
  const rt = new Runtime({ extensions: ['/factory.ts'] }, {});
  rt.uninstallChildren(); rt.namespace = 'actual-bridge'; rt.features.add('command_catalog_v1');
  const s = { id: 7, live: true, controller: new AbortController(), pending: new Set(), errors: [] };
  rt.bind({ context: { resource_owner: owner, workspace: '/work', host: { active_skills: [{name: 'activated-not-available'}] } } }, s, { foreground: true });
  const api = createAPI(rt, 0);
  api.registerCommand('reload', { handler() {} });
  const info = { path: '/pkg/factory.ts', source: 'npm:actual@1.0.0', scope: 'user', origin: 'package' };
  rt.entrypoints = [info.path]; rt.commandSourceInfos = { [info.path]: info };
  let commands = [
    { name: 'actual-bridge:reload', registeredName: 'reload', extension: 'actual-bridge', description: 'reload', source: 'extension', sourceInfo: {path:'/bridge/extension.toml'} },
    { name: 'prompt', description: 'Prompt description', source: 'prompt', sourceInfo: {path:'/prompt.md',source:'auto',scope:'project',origin:'top-level',baseDir:'/work/.pi'} },
    { name: 'skill:available', description: 'Not activated', source: 'skill', sourceInfo: {path:'/skill/SKILL.md',source:'npm:skills',scope:'user',origin:'package'} },
  ];
  const calls = [];
  rt.transport.requestSync = (...args) => { calls.push(args); return {commands}; };
  return { rt, s, api, info, calls, reply(value) {commands=value;}, read() {return rt.scope.run(s, () => api.getCommands());} };
}

test('getCommands uses native availability, resolved invocation names and original sourceInfo, never active skills', () => {
  const h = harness(), commands = h.read();
  assert.deepEqual(commands.map(c=>[c.name,c.source]), [['actual-bridge:reload','extension'],['prompt','prompt'],['skill:available','skill']]);
  assert.deepEqual(commands[0].sourceInfo, h.info);
  assert.equal(Object.hasOwn(commands[0],'description'), false);
  assert.equal(commands[1].sourceInfo.baseDir, '/work/.pi');
  assert.equal(Object.hasOwn(commands[2].sourceInfo,'baseDir'), false);
  assert.deepEqual(h.calls[0][0], 'commands/snapshot');
  assert.deepEqual(h.calls[0][1], {parent_request_id:7,resource_owner:owner});
  assert.equal(h.calls[0][2].parent, 7);
  commands[1].sourceInfo.path = '/corrupted';
  assert.equal(h.read()[1].sourceInfo.path, '/prompt.md');
});

test('getCommands retained callbacks re-read native state after reload and withdrawal and fence dead owners', () => {
  const h = harness(); h.s.live = false;
  assert.equal(h.read().length, 3);
  assert.equal(h.calls[0][2].parent, undefined);
  h.reply([{name:'skill:replacement',source:'skill',sourceInfo:h.info}]);
  assert.equal(h.read()[0].name, 'skill:replacement');
  h.reply([]); assert.deepEqual(h.read(), []);
  h.rt.transport.requestSync = () => { throw Error('native resource lease revoked'); };
  assert.throws(h.read, /lease revoked/);
  h.s.state.alive = false;
  assert.throws(h.read, /not_foreground_owner/);
});

test('getCommands has no pre-bind, unnegotiated, cancelled or draining empty-catalog fallback', () => {
  const h = harness();
  assert.throws(()=>h.api.getCommands(), /not_foreground_owner/);
  h.rt.features.clear(); assert.throws(h.read, /command_catalog_v1/);
  h.rt.features.add('command_catalog_v1'); h.rt.stopping=true;
  assert.throws(h.read, /host is draining/);
  h.rt.stopping=false; h.s.controller.abort(Error('cancelled origin'));
  assert.throws(h.read, /cancelled origin/);
});

test('managed npm and git packages retain their installed baseDir for commands and resources', () => {
  const root=realpathSync(mkdtempSync(join(tmpdir(),'catalog-managed-provenance-')));
  try {
    const agent=join(root,'agent'), work=join(root,'work'); mkdirSync(work);
    const put=(path,text)=>{mkdirSync(join(path,'..'),{recursive:true});writeFileSync(path,text);};
    for (const [source, pkg] of [
      ['npm:actual@1.0.0', join(agent,'npm','node_modules','actual')],
      ['git:github.com/example/catalog', join(agent,'git','github.com','example','catalog')],
    ]) {
      put(join(agent,'settings.json'),JSON.stringify({packages:[source]}));
      put(join(pkg,'package.json'),JSON.stringify({version:'1.0.0',pi:{extensions:['factory.ts'],skills:['skills'],prompts:['prompts']}}));
      const factory=join(pkg,'factory.ts'), skill=join(pkg,'skills','available','SKILL.md'), prompt=join(pkg,'prompts','available.md');
      put(factory,'throw new Error("discovery must not execute factories");');
      put(skill,'---\nname: available\ndescription: Available\n---\nBody'); put(prompt,'Prompt');
      const setup=discoverPiSetup({agentDir:agent,cwd:work});
      assert.deepEqual(setup.diagnostics,[]);
      assert.ok(setup.extensions.includes(factory)); assert.ok(setup.skillsPaths.includes(skill)); assert.ok(setup.promptsPaths.includes(prompt));
      for (const path of [factory,skill,prompt]) assert.deepEqual(setup.sourceInfos[path],{path,source,scope:'user',origin:'package',baseDir:pkg});
    }
  } finally {rmSync(root,{recursive:true,force:true});}
});

test('existing Pi discovery retains package, automatic and explicit sourceInfo semantics without executing factories', () => {
  const root=realpathSync(mkdtempSync(join(tmpdir(),'catalog-provenance-')));
  try {
    const agent=join(root,'agent'), pkg=join(root,'package'), work=join(root,'work');
    const put=(path,text)=>{mkdirSync(join(path,'..'),{recursive:true});writeFileSync(path,text);};
    put(join(agent,'settings.json'),JSON.stringify({packages:[pkg],prompts:['custom.md']}));
    put(join(pkg,'package.json'),JSON.stringify({pi:{skills:['skills']}}));
    const skill=join(pkg,'skills','available','SKILL.md'); put(skill,'---\nname: available\ndescription: Available\n---\nBody');
    const automatic=join(agent,'prompts','automatic.md'); put(automatic,'Prompt');
    const explicit=join(agent,'custom.md'); put(explicit,'Explicit'); mkdirSync(work);
    const setup=discoverPiSetup({agentDir:agent,cwd:work});
    assert.deepEqual(setup.sourceInfos[skill],{path:skill,source:pkg,scope:'user',origin:'package',baseDir:pkg});
    assert.deepEqual(setup.sourceInfos[automatic],{path:automatic,source:'auto',scope:'user',origin:'top-level',baseDir:agent});
    assert.deepEqual(setup.sourceInfos[explicit],{path:explicit,source:'local',scope:'user',origin:'top-level'});
  } finally {rmSync(root,{recursive:true,force:true});}
});

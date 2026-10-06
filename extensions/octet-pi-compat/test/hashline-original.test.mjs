// Opt-in qualification of an unchanged, explicitly supplied hashline source.
// No discovery, credentials, provider calls or installation. JS guards are test
// instrumentation, not an OS sandbox; imports are still trusted reviewed code.
// PI_FLEET_HASHLINE_PATH=/absolute/package/index.ts \
// PI_FLEET_AGENT_DIR=/absolute/pi/agent node --test test/hashline-original.test.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { configure, routeExtensions } from '../configure.mjs';
import { host, launch, root } from './helper.mjs';

const suppliedSource = process.env.PI_FLEET_HASHLINE_PATH;
const suppliedPi = process.env.PI_FLEET_AGENT_DIR;
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const moduleTree = path => path.includes(`${sep}node_modules${sep}`)
  ? path.slice(0, path.lastIndexOf(`${sep}node_modules${sep}`) + '/node_modules'.length) : path;

async function isolated(t, source, pi) {
  const dir = await realpath(await mkdtemp(join(tmpdir(), 'octet-hashline-original-')));
  const home = join(dir, 'home'), agent = join(dir, 'agent'), tmp = join(dir, 'tmp');
  await Promise.all([home, agent, tmp].map(path => mkdir(path)));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const guard = join(dir, 'guard.mjs'), audit = join(dir, 'denied.jsonl');
  await writeFile(audit, '');
  const readRoots = [dir, root, await realpath(join(root, 'node_modules')), moduleTree(source), join(pi, 'install')];
  await writeFile(guard, `
import fs from 'node:fs'; import fsp from 'node:fs/promises';
import cp from 'node:child_process'; import http from 'node:http'; import https from 'node:https';
import net from 'node:net'; import tls from 'node:tls'; import dgram from 'node:dgram';
import { syncBuiltinESMExports } from 'node:module'; import { resolve, sep } from 'node:path'; import { fileURLToPath } from 'node:url';
const append = fs.appendFileSync.bind(fs), canonical = fs.realpathSync.bind(fs);
const dir = ${JSON.stringify(dir)}, roots = ${JSON.stringify(readRoots)};
const configAt = process.argv.indexOf('--config'); if (configAt >= 0) roots.push(canonical(resolve(process.argv[configAt + 1])));
const denied = label => { append(${JSON.stringify(audit)}, JSON.stringify({ denied: label }) + '\\n'); throw new Error('HASHLINE_GUARD: ' + label); };
const inside = (path, root) => path === root || path.startsWith(root.endsWith(sep) ? root : root + sep);
function check(path, write) {
  if (typeof path === 'number') return; // already opened, checked descriptor
  let absolute = resolve(path instanceof URL ? fileURLToPath(path) : String(path));
  try { absolute = canonical(absolute); } catch {}
  if (!(write ? inside(absolute, dir) : roots.some(root => inside(absolute, root)))) denied((write ? 'write ' : 'read ') + absolute);
}
for (const object of [fs, fsp]) {
  for (const name of ['readFile', 'readFileSync', 'createReadStream']) if (typeof object[name] === 'function') {
    const original = object[name].bind(object); object[name] = (path, ...args) => { check(path, false); return original(path, ...args); };
  }
  for (const name of ['writeFile', 'writeFileSync', 'appendFile', 'appendFileSync', 'mkdir', 'mkdirSync', 'rm', 'rmSync', 'unlink', 'unlinkSync', 'chmod', 'chmodSync', 'createWriteStream']) if (typeof object[name] === 'function') {
    const original = object[name].bind(object); object[name] = (path, ...args) => { check(path, true); return original(path, ...args); };
  }
  for (const name of ['rename', 'renameSync', 'copyFile', 'copyFileSync']) if (typeof object[name] === 'function') {
    const original = object[name].bind(object); object[name] = (from, to, ...args) => { check(from, name.startsWith('rename')); check(to, true); return original(from, to, ...args); };
  }
  for (const name of ['open', 'openSync']) if (typeof object[name] === 'function') {
    const original = object[name].bind(object); object[name] = (path, flags, ...args) => {
      const write = typeof flags === 'number' ? Boolean(flags & (fs.constants.O_WRONLY | fs.constants.O_RDWR | fs.constants.O_CREAT | fs.constants.O_TRUNC | fs.constants.O_APPEND)) : /[wa+]/.test(flags ?? 'r');
      check(path, write); return original(path, flags, ...args);
    };
  }
}
for (const [object, names] of [[cp, ['spawn','spawnSync','exec','execSync','execFile','execFileSync','fork']], [http, ['request','get','createServer']], [https, ['request','get','createServer']], [net, ['connect','createConnection','createServer']], [tls, ['connect','createServer']], [dgram, ['createSocket']]])
  for (const name of names) object[name] = () => denied(name);
net.Socket.prototype.connect = () => denied('socket.connect'); net.Server.prototype.listen = () => denied('server.listen');
globalThis.fetch = async () => denied('fetch'); globalThis.WebSocket = class { constructor() { denied('WebSocket'); } };
syncBuiltinESMExports();
`);
  const env = { ...Object.fromEntries(Object.keys(process.env).map(key => [key, undefined])),
    PATH: dirname(process.execPath), HOME: home, USERPROFILE: home, PI_CODING_AGENT_DIR: agent,
    XDG_CONFIG_HOME: home, TMPDIR: tmp, TMP: tmp, TEMP: tmp, JITI_FS_CACHE: 'false',
    NODE_OPTIONS: `--import=${pathToFileURL(guard).href}`, OCTET_PI_AGENT_DIR: pi };
  // Prove the guards are live before trusting their zero-denial report.
  const check = spawnSync(process.execPath, ['--input-type=module', '-e', `
    import assert from 'node:assert/strict'; import { writeFileSync } from 'node:fs';
    import { connect } from 'node:net'; import { spawn } from 'node:child_process';
    for (const fn of [() => connect(1), () => spawn('no-execution'), () => writeFileSync(${JSON.stringify(join(dir, '..', 'must-not-write'))}, '')]) assert.throws(fn, /HASHLINE_GUARD/);
  `], { env, cwd: dir, encoding: 'utf8', timeout: 10000 });
  assert.equal(check.status, 0, check.stderr);
  assert.equal((await readFile(audit, 'utf8')).trim().split('\n').length, 3);
  await writeFile(audit, '');
  return { dir, env, audit };
}

test('original hashline: reviewed installed route executes hashed text, anchored edit and image reads under guards', {
  skip: !suppliedSource || !suppliedPi ? 'explicit PI_FLEET_HASHLINE_PATH and PI_FLEET_AGENT_DIR required' : false,
  timeout: 45000,
}, async t => {
  const source = await realpath(resolve(suppliedSource)), pi = await realpath(resolve(suppliedPi));
  const unchanged = [source, join(dirname(source), 'src/read.ts')];
  const before = await Promise.all(unchanged.map(async path => digest(await readFile(path))));
  const { dir, env, audit } = await isolated(t, source, pi);
  const routed = await routeExtensions([source], { cwd: dir, env });
  assert.deepEqual(routed.map(route => route.route), ['installed'], JSON.stringify(routed));
  const configured = configure({ output: join(dir, 'octet-pi-compat'), extensions: [source], reviewed: true,
    routes: Object.fromEntries(routed.map(route => [route.entry, route.route])), piAgentDir: pi, env, cwd: dir });
  const manifest = await readFile(join(configured.output, 'extension.toml'), 'utf8');
  const grants = JSON.parse(manifest.match(/^builtin_tool_overrides = (.+)$/m)?.[1] ?? 'null');
  assert.deepEqual(grants, ['read']);
  const configPath = join(configured.output, 'bridge.json');
  const config = JSON.parse(await readFile(configPath, 'utf8'));
  const metadata = config.registrations;
  assert.ok(metadata.tools.some(tool => tool.name === 'read'));
  assert.ok(metadata.tools.some(tool => tool.name === 'replace'));
  const h = launch(t, [source], { metadata, config: configPath, env, cwd: dir, auto: false });
  const features = ['builtin_tool_overrides_v1', 'dynamic_tools', 'tool_prompt_metadata_v1', 'artifacts', 'session_entries', 'active_tools', 'lifecycle_events', 'input_transform_v1', 'before_prompt_state_v1'];
  const initialized = await h.request('initialize', { api_version: '0.4', workspace: dir, host,
    extension: { name: 'octet-pi-compat' }, capabilities: { builtin_tool_overrides: grants },
    contributes: { tools: metadata.tools.map(tool => tool.name), commands: metadata.commands.map(command => command.name),
      hooks: metadata.hooks, tool_renderers: metadata.tool_renderers, shortcuts: metadata.shortcuts },
    protocol: { version: '0.4', required_features: ['request_cancellation', 'content_parts'], optional_features: features,
      limits: { max_concurrent_requests: 8 } },
  }).response;
  assert.ok(initialized.result, JSON.stringify(initialized));
  assert.ok(initialized.result.protocol.features.includes('builtin_tool_overrides_v1'));
  // Exercise the real session-start callback, including host-authorized active
  // tool updates; hashline removes native edit and keeps its read replacement.
  const allTools = [...metadata.tools, { name: 'edit', description: 'Native edit', parameters: { type: 'object' } }];
  let active = allTools.map(tool => tool.name);
  const started = h.start();
  for (;;) {
    const frame = await Promise.race([started, h.wait(frame => ['tools/snapshot', 'tools/set_active'].includes(frame.method))]);
    if (!frame.method) break;
    if (frame.method === 'tools/snapshot') h.send({ jsonrpc: '2.0', id: frame.id, result: { all_tools: allTools, active_tools: active } });
    else { active = frame.params.names; h.send({ jsonrpc: '2.0', id: frame.id, result: {} }); }
  }
  assert.ok(active.includes('read')); assert.equal(active.includes('edit'), false);
  const context = h.context({ model_view: { ...host.model_view, input: ['text', 'image'] } });
  const textPath = join(dir, 'text.txt'), imagePath = join(dir, 'small.png');
  await writeFile(textPath, 'alpha\nbeta\ngamma\n');
  await writeFile(imagePath, Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC', 'base64'));
  const text = await h.request('tool/call', { name: 'read', arguments: { path: textPath }, context }).response;
  assert.ok(text.result && !text.result.is_error, JSON.stringify(text));
  const rendered = text.result.content.map(part => part.text).join('\n');
  const anchor = rendered.match(/([^\s│]{3})│beta/);
  assert.ok(anchor, rendered);
  assert.match(rendered, /│alpha/);
  const edited = await h.request('tool/call', { name: 'replace', arguments: { path: textPath,
    remove_from: anchor[1], remove_to: anchor[1], replacement_lines: ['BETA'] }, context }).response;
  assert.ok(edited.result && !edited.result.is_error, JSON.stringify(edited));
  assert.equal(await readFile(textPath, 'utf8'), 'alpha\nBETA\ngamma\n');
  const image = h.request('tool/call', { name: 'read', arguments: { path: imagePath }, context });
  const published = await h.wait(frame => frame.method === 'artifact/publish');
  const upload = published.params;
  assert.equal(upload.mime_type, 'image/png'); assert.equal(upload.data.encoding, 'base64');
  const bytes = Buffer.from(upload.data.data, 'base64');
  assert.equal(bytes.length, upload.size); assert.equal(digest(bytes), upload.sha256);
  assert.deepEqual([...bytes.subarray(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10]);
  h.send({ jsonrpc: '2.0', id: published.id, result: { artifact_id: 'host-accepted-hashline-image' } });
  const imaged = await image.response;
  assert.ok(imaged.result && !imaged.result.is_error, JSON.stringify(imaged));
  assert.ok(imaged.result.content.some(part => part.type === 'image' && part.artifact_id === 'host-accepted-hashline-image'));
  await h.close();
  assert.equal(await readFile(audit, 'utf8'), '', 'original source attempted denied file/network/process access');
  assert.deepEqual(await Promise.all(unchanged.map(async path => digest(await readFile(path)))), before, 'original source changed');
  t.diagnostic(JSON.stringify({ factory_sha256: before[0], read_helper_sha256: before[1],
    artifact_sha256: upload.sha256, builtin_grants: grants, denied_operations: 0 }));
});

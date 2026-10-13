import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, readFileSync, copyFileSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { harness, initialize, request } from './harness.mjs';

const packageRoot = fileURLToPath(new URL('../process', import.meta.url));
test('offline packed SDK is runnable by an installed TypeScript author with generated local manifest', {timeout: 20_000}, async t => {
  const staging = mkdtempSync(join(tmpdir(), "octet-ts-runtime-'quoted-"));
  t.after(() => rmSync(staging, {recursive: true, force: true}));
  const consumer = join(staging, 'typescript-hello'); mkdirSync(consumer);
  const packed = JSON.parse(execFileSync('npm', ['pack', '--json', '--pack-destination', staging], {cwd: packageRoot, encoding: 'utf8'}));
  const paths = packed[0].files.map(file => file.path);
  for (const file of ['index.mjs', 'index.d.mts', 'schema.mjs', 'cli.mjs', 'README.md']) assert(paths.includes(file));
  assert(!paths.some(file => file.startsWith('tests/')));
  writeFileSync(join(consumer, 'package.json'), JSON.stringify({name: 'typescript-hello', private: true, type: 'module'}));
  execFileSync('npm', ['install', '--offline', '--ignore-scripts', '--no-package-lock', join(staging, packed[0].filename)], {cwd: consumer, stdio: 'pipe'});
  const source = join(consumer, 'extension.ts');
  writeFileSync(source, readFileSync(resolve(packageRoot, '../../../examples/extensions/typescript-hello/extension.ts')));
  const installedCli = join(consumer, 'node_modules/@skaft-software/octet-extension-sdk/cli.mjs');
  const manifest = join(consumer, 'extension.toml');
  execFileSync(process.execPath, [installedCli, 'manifest', source, '--name', 'typescript-hello', '--version', '0.1.0', '--out', manifest], {encoding: 'utf8', stdio: 'pipe'});
  const text = readFileSync(manifest, 'utf8');
  assert.match(text, /api_version = "0.4"/); assert.match(text, /tools = \["text_stats"\]/);
  assert.match(text, /command = .*\.octet-launcher\.sh/); assert.match(text, /args = \[\]/);
  const launcher = join(consumer, '.octet-launcher.sh');
  const launch = readFileSync(launcher, 'utf8');
  assert(launch.startsWith('#!/bin/sh\n')); assert(launch.includes('exec '));
  // Model actual host staging: relocate only the generated script, never Node.
  const stagedLauncher = join(staging, 'staged-launcher.sh');
  copyFileSync(launcher, stagedLauncher); chmodSync(stagedLauncher, 0o700);
  const overwrite = spawnSync(process.execPath, [installedCli, 'manifest', source, '--name', 'typescript-hello', '--version', '0.1.0', '--out', manifest], {encoding: 'utf8'});
  assert.equal(overwrite.status, 1); assert.equal(readFileSync(manifest, 'utf8'), text);
  // The harness launches the installed author (whose import resolves the packed SDK),
  // while its recorder remains test-only and does not replace runtime code.
  const h = harness(t, {source, launcher: stagedLauncher});
  const params = initialize({contributes: {tools: ['text_stats'], commands: []}});
  const result = await h.ready(params); assert.equal(result.tools[0].name, 'text_stats');
  const ctx = {workspace: consumer, host: {}, execution_scope: null};
  h.send(request(2, 'tool/call', {name: 'text_stats', arguments: {text: 'hello world\n🦀'}, context: ctx}));
  assert.equal((await h.reply(2)).result.content[0].text, 'characters=13 words=3 lines=2 utf8_bytes=16');
  h.send(request(3, 'tool/call', {name: 'text_stats', arguments: {text: 'cancel', delayMs: 2000}, context: ctx}));
  await h.progress(3); h.send({jsonrpc: '2.0', method: '$/cancelRequest', params: {id: 3}});
  assert.equal((await h.reply(3)).error.code, -32800); await h.stop();
});

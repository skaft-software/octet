// The per-Pi-release manifests are the adapter's only description of a Pi
// release: a route map (specifier -> deep module) and a classification table
// (symbol -> inert|authority, with the deep module that serves an inert one).
// These tests keep them consistent with the shims, the resolution rule, and —
// when a matching managed Pi install is present — with the release itself.
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync, mkdirSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { root } from './helper.mjs';
import {
  PI_MANIFEST_VERSIONS, manifestFile, piManifest, piRelease, piReleaseSupported, piReleaseTested, piSupportedRange,
} from '../lib/pi-manifests.mjs';
import { PI_SUPPORTED_RANGE, PI_VERSION, PI_MODULES, PI_PREFIXES, emulatedPiAliases, piModule } from '../lib/pi-modules.mjs';
import * as ai from '../shims/ai.mjs';
import * as coding from '../shims/coding-agent.mjs';
import * as tui from '../shims/tui.mjs';
import * as core from '../shims/agent-core.mjs';
import * as providers from '../shims/ai-providers.mjs';

const manifests = Object.fromEntries(PI_MANIFEST_VERSIONS.map(version => [version, piManifest(version)]));
const shims = { ai, 'coding-agent': coding, tui, 'agent-core': core, 'ai-providers': providers };
const installRoot = version => join(homedir(), '.pi', 'agent', 'install', 'releases', version, 'node_modules', '@earendil-works');
const installed = version => existsSync(installRoot(version));
const packageDir = { ai: 'pi-ai', 'coding-agent': 'pi-coding-agent', tui: 'pi-tui', 'agent-core': 'pi-agent-core', 'ai-providers': 'pi-ai' };
const shimEntry = pathToFileURL(join(root, 'shims/ai.mjs')).href;

// A managed Pi agent directory that names an already-installed release.
function agentDirFor(t, version) {
  const agent = mkdtempSync(join(tmpdir(), `octet-pi-${version}-`));
  t.after(() => rmSync(agent, { recursive: true, force: true }));
  mkdirSync(join(agent, 'install', 'releases'), { recursive: true });
  symlinkSync(join(homedir(), '.pi', 'agent', 'install', 'releases', version), join(agent, 'install', 'releases', version));
  writeFileSync(join(agent, 'install', 'current-version'), `${version}\n`);
  return agent;
}
const runShim = (agent, body) => spawnSync(process.execPath, ['--input-type=module', '-e', `
  const ai = await import(${JSON.stringify(shimEntry)});
  ${body}`], { encoding: 'utf8', env: { ...process.env, OCTET_PI_AGENT_DIR: agent } });

test('every shipped manifest parses as schema 1 for its own release', () => {
  assert.deepEqual(PI_MANIFEST_VERSIONS, ['1.0.2', '1.5.0']);
  for (const [version, manifest] of Object.entries(manifests)) {
    assert.equal(manifest.schema, 1);
    assert.equal(manifest.pi, version);
    assert.match(manifest.reference.repository, /pi/);
    assert.deepEqual(manifest.scopes, ['@earendil-works', '@mariozechner']);
    assert.equal(manifest.typebox.package, 'typebox');
    assert.deepEqual(manifest.typebox.spellings, ['typebox', '@sinclair/typebox']);
    assert.equal(readFileSync(manifestFile(version), 'utf8'), JSON.stringify(manifest, null, 2) + '\n');
  }
});

test('manifest routes and symbol routes name deep modules, never a package root', () => {
  for (const [version, manifest] of Object.entries(manifests)) {
    for (const [specifier, route] of Object.entries(manifest.routes)) {
      assert.ok(['pi-coding-agent', 'pi-agent-core', 'pi-tui', 'pi-ai'].includes(route.package), `${version} ${specifier}`);
      assert.ok(route.export.startsWith('.'), `${version} ${specifier}`);
      assert.ok(!specifier.startsWith('@'), `${version} ${specifier} is stored without its scope`);
      if (route.shim) assert.ok(Object.hasOwn(shims, route.shim), `${version} ${specifier} shim ${route.shim}`);
      if (route.view) {
        assert.match(route.view.module, /^dist\/.+\.js$/, `${version} ${specifier} view`);
        assert.ok(!route.view.module.endsWith('/index.js'), `${version} ${specifier} view must not be a package root`);
      }
    }
    for (const [pkg, entries] of Object.entries(manifest.symbols)) {
      for (const [symbol, entry] of Object.entries(entries)) {
        assert.ok(['inert', 'authority'].includes(entry.surface), `${version} ${pkg}.${symbol} surface`);
        if (entry.route == null) continue;
        assert.match(entry.route, /\.js$/, `${version} ${pkg}.${symbol} route`);
        assert.ok(!entry.route.endsWith('/index.js') && entry.route !== 'index.js', `${version} ${pkg}.${symbol} route must be a specific module`);
        assert.equal(entry.surface, 'inert', `${version} ${pkg}.${symbol} routes only inert symbols`);
      }
    }
  }
});

test('every shim export is classified, and no shim binding is undefined', () => {
  for (const [version, manifest] of Object.entries(manifests)) {
    for (const [pkg, namespace] of Object.entries(shims)) {
      for (const name of Object.keys(namespace)) {
        if (name === 'default') continue;
        assert.ok(manifest.symbols[pkg]?.[name], `${version}: ${pkg}.${name} is exported by the shim but not classified`);
        assert.notEqual(namespace[name], undefined, `${pkg}.${name} is undefined`);
      }
    }
  }
});

test('the supported range and the emulated surface come from the manifests', () => {
  assert.equal(PI_SUPPORTED_RANGE, '>=1.0.2 <2.0.0');
  assert.equal(piSupportedRange(), PI_SUPPORTED_RANGE);
  assert.equal(PI_VERSION, '1.5.0');
  assert.deepEqual(PI_PREFIXES, ['@earendil-works', '@mariozechner']);
  assert.deepEqual(Object.keys(PI_MODULES).sort(), Object.keys(manifests['1.5.0'].routes).sort());
  for (const prefix of PI_PREFIXES) for (const specifier of Object.keys(PI_MODULES)) assert.match(emulatedPiAliases()[`${prefix}/${specifier}`], /\.mjs$/);
  assert.equal(piModule('@earendil-works/pi-ai/models').shim, 'ai');
  assert.equal(piModule('@earendil-works/pi-ai/providers/anthropic').unmapped, true);
});

test('release resolution: exact manifest, diagnosed nearest older, unsupported outside it', () => {
  assert.equal(piRelease('1.0.2').exact, true);
  assert.equal(piRelease('1.5.0').exact, true);
  assert.equal(piReleaseTested('1.5.0'), true);
  const untested = piRelease('1.6.0');
  assert.equal(untested.exact, false);
  assert.equal(untested.substitutedFrom, '1.5.0');
  assert.equal(piReleaseTested('1.6.0'), false);
  assert.equal(piRelease('1.9.9').substitutedFrom, '1.5.0');
  for (const version of ['1.0.1', '1.0.0', '2.0.0', '0.99.2', '1.4.0-rc.1', '01.4.0', '1.4', '', 'v1.5.0']) {
    assert.equal(piRelease(version), null, version);
    assert.equal(piReleaseSupported(version), false, version);
  }
});

test('the 1.5.0 manifest captures what moved between releases', () => {
  const delta = manifests['1.5.0'].delta;
  assert.equal(delta.from, '1.0.2');
  assert.deepEqual(delta.removed['1.5.0'].ai.sort(), ['AssistantMessageFrameEncoder', 'reduceAssistantMessageFrames']);
  assert.deepEqual(delta.added['1.5.0'].ai.sort(), ['StreamingJsonParser', 'ToolCallArgumentsStream']);
  assert.deepEqual(delta.added['1.5.0'].tui.sort(), ['getSelectPageIndex', 'replaceControlCharacters']);
  assert.ok(delta.added['1.0.3-1.4.0']['coding-agent'].includes('formatReadLineRange'));
  assert.deepEqual(delta.moved['1.5.0'].map(move => move.symbol), ['Type', 'StringEnum', '@earendil-works/pi-ai']);
  // The frame helpers are routed in 1.0.2 and gone in 1.5.0: an inert symbol
  // without a route is a removal, not an undefined binding.
  for (const [version, route] of [['1.0.2', 'utils/assistant-message-frame.js'], ['1.5.0', null]]) {
    for (const symbol of ['reduceAssistantMessageFrames', 'AssistantMessageFrameEncoder']) {
      assert.equal(manifests[version].symbols.ai[symbol].route, route, `${version} ${symbol}`);
    }
  }
  assert.equal(manifests['1.5.0'].symbols.ai.Type.moved, '1.5.0');
  assert.equal(manifests['1.5.0'].symbols.ai.StringEnum.moved, '1.5.0');
  assert.match(manifests['1.5.0'].routes['pi-ai'].view.module, /pi-ai-extension-view\.js$/);
  assert.equal(manifests['1.5.0'].delta.validation['1.5.0'].length, 4);
  // A shim-owned fallback is only claimed where the shim carries one.
  for (const manifest of Object.values(manifests)) {
    const fallbacks = Object.entries(manifest.symbols).flatMap(([pkg, entries]) => Object.entries(entries)
      .filter(([, entry]) => entry.fallback).map(([symbol]) => `${pkg}.${symbol}`));
    assert.deepEqual(fallbacks.sort(), ['ai.validateToolArguments', 'ai.validateToolCall']);
  }
});

test('a routed symbol resolves to a file that exports it on the matching install', t => {
  for (const [version, manifest] of Object.entries(manifests)) {
    if (!installed(version)) { t.diagnostic(`no managed Pi ${version}; skipped file resolution`); continue; }
    for (const [pkg, entries] of Object.entries(manifest.symbols)) {
      for (const [symbol, entry] of Object.entries(entries)) {
        if (entry.surface !== 'inert' || !entry.route) continue;
        const file = join(installRoot(version), packageDir[pkg], 'dist', entry.route);
        assert.ok(existsSync(file), `${version} ${pkg}.${symbol} -> dist/${entry.route}`);
        const source = readFileSync(file, 'utf8');
        const exported = new RegExp(`export (async )?(function|const|let|var|class) ${symbol}\\b|\\b${symbol}\\s*[,}]`).test(source);
        // A symbol the release predates is routed but absent by record: the
        // manifest says when it arrived, so the shim fails by name there.
        if (!exported) assert.ok(entry.added && !entry.added.startsWith(version.slice(0, 3)),
          `${version} ${pkg}.${symbol}: dist/${entry.route} does not export it and the manifest does not record it as later`);
        assert.ok(exported || entry.added, `${version} ${pkg}.${symbol}`);
      }
    }
  }
});

test('inert helpers bind the release the manifest describes, and fail by name otherwise', t => {
  if (!installed('1.5.0')) return t.diagnostic('no managed Pi 1.5.0; skipped the absent-symbol diagnostics');
  const absent = runShim(agentDirFor(t, '1.5.0'), `
    for (const name of ['reduceAssistantMessageFrames', 'AssistantMessageFrameEncoder']) {
      try { name.endsWith('Encoder') ? new ai[name]() : ai[name]([]); console.log(name, 'bound'); }
      catch (error) { console.log(error.code, error.message); }
    }`);
  assert.equal(absent.status, 0, absent.stderr);
  assert.match(absent.stdout, /-32601 unsupported_feature pi-ai\.reduceAssistantMessageFrames: Pi 1\.5\.0 does not ship it \(removed in 1\.5\.0\): the experimental agent harness .* was removed/);
  assert.match(absent.stdout, /-32601 unsupported_feature pi-ai\.AssistantMessageFrameEncoder: Pi 1\.5\.0 does not ship it/);
  // A symbol the release does ship comes from Pi's own deep module.
  const present = runShim(agentDirFor(t, '1.5.0'), `
    const parsed = ai.parseToolCallArguments('{"path":"a"}');
    console.log(JSON.stringify(parsed));`);
  assert.equal(present.status, 0, present.stderr);
  assert.match(present.stdout, /^\{"arguments":\{"path":"a"\}\}/m);

  if (!installed('1.0.2')) return t.diagnostic('no managed Pi 1.0.2; skipped the routed-helper path');
  const legacy = runShim(agentDirFor(t, '1.0.2'), `
    const encoder = new ai.AssistantMessageFrameEncoder();
    console.log(typeof encoder.encode, typeof ai.reduceAssistantMessageFrames);
    try { ai.parseToolCallArguments('{}'); } catch (error) { console.log(error.code, error.message); }`);
  assert.equal(legacy.status, 0, legacy.stderr);
  assert.match(legacy.stdout, /^function function$/m);
  assert.match(legacy.stdout, /-32032 pi_compat_helper_absent pi-ai\.parseToolCallArguments: Pi 1\.0\.2 routes it to dist\/utils\/json-parse\.js, which does not export it/);
});

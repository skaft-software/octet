// Opt-in, unchanged external factories. Registration/import evidence only:
// this does not qualify MCP execution or Pi CLI subagents on the native host.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import { root } from './helper.mjs';

// Never discover factories in HOME. Setting these paths authorizes the reviewed
// import, without granting access to the user's MCP configuration/credentials.
const mcp = process.env.PI_FLEET_MCP_PATH;
const subagents = process.env.PI_FLEET_SUBAGENTS_PATH;
// Subagents also needs the explicitly approved installed-Pi route (keyText).
// Only its managed release is used; HOME/agent state remains isolated below.
const installedAgent = process.env.PI_FLEET_AGENT_DIR;
function isolatedImport(t, source, installed = false) {
  const dir = mkdtempSync(join(tmpdir(), 'octet-fleet-original-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const agent = join(dir, 'agent'), temporary = join(dir, 'tmp');
  mkdirSync(agent); mkdirSync(temporary);
  writeFileSync(join(agent, 'mcp.json'), JSON.stringify({ mcpServers: {} }));
  const guard = join(dir, 'guard.mjs');
  writeFileSync(guard, `
    import childProcess from 'node:child_process';
    import net from 'node:net';
    import tls from 'node:tls';
    import http from 'node:http';
    import https from 'node:https';
    import dgram from 'node:dgram';
    import { syncBuiltinESMExports } from 'node:module';
    const denied = () => { throw new Error('import-only probe forbids network and child processes'); };
    for (const key of ['spawn','spawnSync','exec','execSync','execFile','execFileSync','fork']) childProcess[key] = denied;
    net.Socket.prototype.connect = denied; net.Server.prototype.listen = denied;
    net.connect = net.createConnection = tls.connect = denied;
    http.request = http.get = https.request = https.get = denied;
    dgram.createSocket = denied; globalThis.fetch = denied; globalThis.WebSocket = denied;
    syncBuiltinESMExports();
  `);
  const config = join(dir, 'bridge.json');
  writeFileSync(config, JSON.stringify({ extensions: [resolve(source)],
    ...(installed ? { pi_runtime: 'installed', pi_agent_dir: resolve(installedAgent) } : {}) }));
  const result = spawnSync(process.execPath, ['--import', pathToFileURL(guard).href, join(root, 'runner.mjs'), '--inspect', '--config', config], {
    cwd: dir, encoding: 'utf8', timeout: 20_000, maxBuffer: 1024 * 1024,
    env: { PATH: dirname(process.execPath), HOME: dir, USERPROFILE: dir, TMPDIR: temporary, TMP: temporary, TEMP: temporary,
      PI_CODING_AGENT_DIR: agent, PI_MCP_CONFIG_MODE: 'exclusive', MCP_DIRECT_TOOLS: '__none__',
      PI_SUBAGENTS_TEMP_ROOT: join(temporary, 'subagents'), JITI_FS_CACHE: 'false' },
  });
  assert.equal(result.status, 0, result.stderr || String(result.error));
  return JSON.parse(result.stdout).result;
}

test('unchanged MCP factory imports with truly absent unregisterTool and empty exclusive configuration', { skip: !mcp && 'set PI_FLEET_MCP_PATH to the reviewed original factory' }, t => {
  const metadata = isolatedImport(t, mcp);
  assert.ok(metadata.tools.some(tool => tool.name === 'mcp'));
});

test('unchanged subagents factory imports without stripping lane pattern or deprecated annotation', { skip: (!subagents || !installedAgent) && 'set PI_FLEET_SUBAGENTS_PATH and PI_FLEET_AGENT_DIR to the reviewed factory and Pi 1.0.2 install' }, t => {
  const metadata = isolatedImport(t, subagents, true);
  const tool = metadata.tools.find(tool => tool.name === 'subagent');
  assert.ok(tool);
  assert.equal(tool.parameters.properties.lane.properties.key.pattern, '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$');
  assert.equal(tool.parameters.properties.acceptance.anyOf[1].deprecated, true);
});

// Pi 1.0.2 MCP registry semantics, adapted from core/mcp-servers.ts (MIT).
// Connections belong to the resident native octet-mcp bridge, never this module.
import { bounded, invalid, plainJSON, rpcError } from './errors.mjs';

const registries = new WeakMap();
const exposures = new Set(['codemode', 'deferred', 'direct', 'hidden']);
const loopback = new Set(['localhost', '127.0.0.1', '[::1]']);
const record = value => typeof value === 'object' && value !== null && !Array.isArray(value);
const strings = value => record(value) && Object.values(value).every(v => typeof v === 'string');
const alias = value => value === 'codemode-deferred' ? 'codemode' : value;
const namespace = name => name.replaceAll('-', '_');
const url = value => typeof value === 'string' && URL.canParse(value) ? new URL(value) : undefined;

function validateOAuth(value) {
  if (value === undefined) return;
  if (!record(value)) invalid('MCP oauth must be an object');
  for (const key of ['clientId', 'clientSecret', 'scope']) {
    if (value[key] !== undefined && typeof value[key] !== 'string') invalid(`MCP oauth.${key}`);
  }
  const port = value.callbackPort;
  if (port !== undefined && (!Number.isInteger(port) || port < 1 || port > 65535)) invalid('MCP oauth.callbackPort');
  const callback = url(value.callbackUrl);
  if (value.callbackUrl !== undefined) {
    if (!callback || callback.protocol !== 'http:' || !loopback.has(callback.hostname)
      || callback.search || callback.hash) invalid('MCP oauth.callbackUrl');
    if (callback.port && port !== undefined && Number(callback.port) !== port) invalid('MCP oauth callback port mismatch');
  }
  if (value.clientName !== undefined && (typeof value.clientName !== 'string' || !value.clientName.trim())) invalid('MCP oauth.clientName');
  if (value.clientRegistration !== undefined && value.clientRegistration !== 'dcr') {
    if (value.clientRegistration !== 'cimd') invalid('MCP oauth.clientRegistration');
    if (value.clientId !== undefined || value.clientName !== undefined) invalid('MCP cimd client settings');
    if (callback && (callback.hostname === '[::1]' || callback.pathname !== '/callback')) invalid('MCP cimd callback');
  }
  if (value.authServerMetadataUrl !== undefined) {
    const metadata = url(value.authServerMetadataUrl);
    if (!metadata || !(metadata.protocol === 'https:' || metadata.protocol === 'http:' && loopback.has(metadata.hostname))) {
      invalid('MCP oauth.authServerMetadataUrl');
    }
  }
}

export function validateMcpServerConfig(name, raw) {
  bounded(name, 'MCP server name', 128);
  if (!/^[A-Za-z0-9_-]+$/.test(name) || !record(raw)) invalid('MCP server name/config');
  // Pi clones registrations, including fields unknown to its validator. Do not
  // mutate the caller or accidentally resolve credentials/environment here.
  const value = structuredClone(raw);
  if (value.exposure !== undefined) value.exposure = alias(value.exposure);
  if (record(value.toolExposure)) value.toolExposure = Object.fromEntries(
    Object.entries(value.toolExposure).map(([tool, exposure]) => [tool, alias(exposure)]));
  if (value.exposure !== undefined && !exposures.has(value.exposure)) invalid('MCP exposure');
  if (value.toolExposure !== undefined && (!record(value.toolExposure)
    || Object.values(value.toolExposure).some(v => !exposures.has(v)))) invalid('MCP toolExposure');
  if (value.enabled !== undefined && typeof value.enabled !== 'boolean') invalid('MCP enabled');
  if (value.description !== undefined && typeof value.description !== 'string') invalid('MCP description');
  if (value.timeout !== undefined && (typeof value.timeout !== 'number' || !(value.timeout > 0))) invalid('MCP timeout');
  if (value.type === 'sse') invalid('MCP legacy SSE transport');
  if (typeof value.url === 'string' && [undefined, 'http', 'streamable-http'].includes(value.type)) {
    const endpoint = url(value.url);
    if (!endpoint || !['http:', 'https:'].includes(endpoint.protocol)) invalid('MCP HTTP URL');
    if (value.headers !== undefined && !strings(value.headers)) invalid('MCP headers');
    validateOAuth(value.oauth);
    if (value.auth !== undefined) {
      if (!record(value.auth) || typeof value.auth.provider !== 'string' || !value.auth.provider) invalid('MCP auth.provider');
      if (endpoint.protocol !== 'https:' && !loopback.has(endpoint.hostname)) invalid('MCP auth requires HTTPS or loopback');
    }
    return value;
  }
  if (typeof value.command === 'string' && [undefined, 'stdio'].includes(value.type)) {
    if (value.args !== undefined && !(Array.isArray(value.args) && value.args.every(arg => typeof arg === 'string'))) invalid('MCP args');
    if (value.env !== undefined && !strings(value.env)) invalid('MCP env');
    if (value.cwd !== undefined && typeof value.cwd !== 'string') invalid('MCP cwd');
    return value;
  }
  invalid('MCP config needs command (stdio) or URL (HTTP)');
}

function registry(runtime, state) {
  let seeds = registries.get(runtime);
  if (!seeds) { seeds = new Map(); registries.set(runtime, seeds); }
  if (!state) return seeds;
  state.piMcpServers ??= new Map([...seeds].map(([name, server]) => [name, structuredClone(server)]));
  return state.piMcpServers;
}
const list = servers => [...servers.values()].map(server => structuredClone(server));
export const hasMcpRegistrations = runtime => Boolean(registries.get(runtime)?.size);

async function connect(runtime, store, servers) {
  runtime.assertOwner(store); store.controller.signal.throwIfAborted();
  runtime.require('mcp_registration_v1');
  const reply = await runtime.hostCall('mcp/replace', {
    resource_owner: store.state.owner, servers: plainJSON(servers, 'MCP registrations', 262144),
  }, store);
  if (!record(reply) || !Array.isArray(reply.errors) || !Array.isArray(reply.shadowed)
    || !record(reply.changes)) invalid('MCP native acknowledgement');
  for (const error of reply.errors) {
    // Config/parser/server text must not reach diagnostics, especially env or
    // headers. Expose only a fixed diagnostic code and a known registered name.
    if (!record(error) || !servers.some(server => server.name === error.name)
      || typeof error.code !== 'string' || !/^[a-z_]{1,64}$/.test(error.code)) invalid('MCP native diagnostic');
    runtime.backgroundError(rpcError(-32601, `MCP server ${error.name}: ${error.code}`));
  }
}

function changed(runtime, store, servers) {
  const state = store.state;
  const previous = state.piMcpTail ?? Promise.resolve();
  const work = previous.catch(() => {}).then(async () => {
    runtime.assertOwner(store); store.controller.signal.throwIfAborted();
    if (runtime.events.get('mcp_servers_change')?.length) {
      await runtime.runEvent('mcp_servers_change', { type: 'mcp_servers_change', servers }, store);
    } else {
      await connect(runtime, store, servers);
    }
  });
  state.piMcpTail = work;
  runtime.track(work, store); // The public mutation is synchronous void.
}

export function mcpAPI(runtime, factory) {
  const active = () => {
    const store = runtime.loaded ? runtime.current(factory) : undefined;
    return { store, servers: registry(runtime, store?.state) };
  };
  const extensionPath = runtime.config.extensions[factory];
  return {
    registerMcpServer(name, config) {
      const { store, servers } = active();
      const validated = validateMcpServerConfig(name, config);
      const owner = servers.get(name)?.extensionPath;
      if (owner !== undefined && owner !== extensionPath) invalid(`MCP server ${name} is owned by another extension`);
      for (const server of servers.values()) {
        if (server.name !== name && namespace(server.name) === namespace(name)) invalid(`MCP server ${name} namespace collision`);
      }
      if (!servers.has(name) && servers.size >= 32) invalid('bounds_exceeded MCP registrations');
      servers.set(name, { name, config: validated, extensionPath });
      if (store) changed(runtime, store, list(servers));
    },
    unregisterMcpServer(name) {
      const { store, servers } = active();
      if (servers.get(name)?.extensionPath !== extensionPath) return;
      servers.delete(name);
      if (store) changed(runtime, store, list(servers));
    },
    getMcpServers() { return list(active().servers); },
  };
}

// Load-time registrations connect at session_start, without a synthetic change
// event. A custom Pi MCP consumer owns its session_start callback instead.
export async function startMcpSession(runtime, store) {
  const servers = list(registry(runtime, store.state));
  if (servers.length && !runtime.events.get('mcp_servers_change')?.length) await connect(runtime, store, servers);
}
export function retireMcpSession(state) {
  state.piMcpServers?.clear();
  // Native owner retirement/crash/reload also removes connections. Never send
  // cleanup through a fabricated or already retired JS parent context.
}

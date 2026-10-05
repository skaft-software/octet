import { createHash } from 'node:crypto';
import { constants, lstatSync, openSync, fstatSync, readSync, closeSync, unlinkSync } from 'node:fs';
import { lstat, open, unlink } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { bounded, fields, invalid, plainJSON, unsupported } from './errors.mjs';

// Public Pi names end at this boundary; the Rust wire stays canonical.
export function validateTool(tool) {
  fields(tool, ['name', 'label', 'description', 'parameters', 'execute', 'promptSnippet', 'promptGuidelines',
    'outputSchema', 'prepareArguments', 'renderCall', 'renderResult', 'renderShell', 'annotations',
    'defaultActive', 'executionMode', 'exposure', 'namespace', 'constrainedSampling', 'prepareLoadout'], 'tool');
  bounded(tool.name, 'tool name', 128);
  if (!/^[A-Za-z_][A-Za-z0-9_.:-]*$/.test(tool.name)) invalid('tool name');
  bounded(tool.description, 'tool description', 4096);
  if (!tool.description.trim() || !tool.parameters || typeof tool.parameters !== 'object' || Array.isArray(tool.parameters) || typeof tool.execute !== 'function') invalid('tool definition');
  if (tool.label !== undefined) bounded(tool.label, 'tool label', 128);
  if (tool.promptSnippet !== undefined) bounded(tool.promptSnippet, 'tool promptSnippet', 1024);
  if (tool.promptGuidelines !== undefined) {
    if (!Array.isArray(tool.promptGuidelines) || tool.promptGuidelines.length > 16) invalid('tool promptGuidelines must contain at most 16 strings');
    for (const value of tool.promptGuidelines) bounded(value, 'tool promptGuidelines entry', 1024);
  }
  for (const key of ['renderCall', 'renderResult']) if (tool[key] !== undefined && typeof tool[key] !== 'function') invalid(`tool.${key}`);
  if (tool.renderShell !== undefined && !['default', 'self'].includes(tool.renderShell)) invalid('tool.renderShell');
  if (tool.defaultActive !== undefined && typeof tool.defaultActive !== 'boolean') invalid('tool.defaultActive');
  if (tool.outputSchema !== undefined && (!tool.outputSchema || typeof tool.outputSchema !== 'object' || Array.isArray(tool.outputSchema))) invalid('tool.outputSchema');
  for (const key of ['prepareArguments', 'prepareLoadout']) if (tool[key] !== undefined && typeof tool[key] !== 'function') invalid(`tool.${key}`);
  if (tool.exposure !== undefined && tool.exposure !== 'direct') unsupported('tool.exposure', 'non-direct callable and declared surfaces are not bound');
  if (tool.executionMode !== undefined && tool.executionMode !== 'sequential') unsupported('tool.executionMode', 'the host keeps extension effects exclusive');
  // These are consumed by getAllTools, never used as verified effect or replay declarations.
  if (tool.annotations !== undefined) {
    fields(tool.annotations, ['readOnlyHint', 'destructiveHint', 'idempotentHint', 'openWorldHint'], 'tool annotations');
    for (const value of Object.values(tool.annotations)) if (typeof value !== 'boolean') invalid('tool annotations must be boolean hints');
  }
  if (tool.namespace !== undefined) {
    fields(tool.namespace, ['name', 'description', 'instructions'], 'tool namespace');
    bounded(tool.namespace.name, 'tool namespace name', 128);
    for (const key of ['description', 'instructions']) if (tool.namespace[key] !== undefined) bounded(tool.namespace[key], `tool namespace ${key}`, 4096);
  }
  if (tool.constrainedSampling !== undefined && tool.constrainedSampling !== false) {
    const sampling = tool.constrainedSampling;
    if (sampling?.type === 'json_schema') {
      fields(sampling, ['type', 'strict'], 'tool constrainedSampling');
      if (!['prefer', 'require'].includes(sampling.strict)) invalid('tool constrainedSampling strict');
    } else if (sampling?.type === 'grammar') {
      fields(sampling, ['type', 'variants'], 'tool constrainedSampling');
      fields(sampling.variants, ['openai_lark', 'openai_regex'], 'tool grammar variants');
      for (const value of Object.values(sampling.variants)) bounded(value, 'tool grammar', 262144, { controls: true });
    } else invalid('tool constrainedSampling type');
  }
}

export function toolWire(name, { definition: d }) {
  return { name, description: d.description, parameters: JSON.parse(JSON.stringify(d.parameters)),
    nested_execution: true,
    ...(d.prepareArguments === undefined ? {} : { prepare_arguments: true }),
    ...(d.promptSnippet === undefined ? {} : { prompt_snippet: d.promptSnippet }),
    ...(d.promptGuidelines === undefined ? {} : { prompt_guidelines: [...d.promptGuidelines] }),
    ...(d.outputSchema === undefined ? {} : { output_schema: JSON.parse(JSON.stringify(d.outputSchema)) }),
    ...(d.defaultActive === undefined ? {} : { default_active: d.defaultActive }),
    ...(!d.constrainedSampling ? {} : { constrained_sampling: plainJSON(d.constrainedSampling, 'tool constrainedSampling', 262144) }),
  };
}

export function registerTool(runtime, factory, definition) {
  validateTool(definition);
  if (!runtime.tools.has(definition.name) && runtime.tools.size >= 256) invalid('tool registration limit');
  const entry = { definition, factory };
  if (!runtime.loaded) { runtime.tools.set(definition.name, entry); return; }
  runtime.require('dynamic_tools');
  const store = runtime.current(factory);
  // Pi registration is synchronous void. Reuse the transport worker so a
  // following getter sees the host ACK, not an optimistic local catalog.
  const result = runtime.transport.requestSync('tools/register', { tools: [toolWire(definition.name, entry)] }, { signal: store.controller.signal });
  fields(result, ['revision', 'tools'], 'tool registration acknowledgement');
  if (!Number.isSafeInteger(result.revision) || result.revision < 0 || !Array.isArray(result.tools) || !result.tools.includes(definition.name)) invalid('host did not publish registered tool');
  runtime.tools.set(definition.name, entry);
}

export function toolSnapshot(runtime, factory) {
  runtime.require('active_tools');
  const store = runtime.current(factory);
  const result = runtime.transport.requestSync('tools/snapshot', {
    parent_request_id: store.id, resource_owner: store.state.owner,
  }, { parent: store.live ? store.id : undefined, signal: store.controller.signal });
  fields(result, ['active_tools', 'all_tools'], 'tools snapshot');
  if (!Array.isArray(result.active_tools) || !Array.isArray(result.all_tools)) invalid('tools snapshot arrays');
  return result;
}

export function getAllTools(runtime, factory, snapshot = toolSnapshot(runtime, factory)) {
  return snapshot.all_tools.map(tool => {
    const local = runtime.tools.get(tool.name);
    return { ...tool,
      ...(local?.definition.promptGuidelines === undefined ? {} : { promptGuidelines: [...local.definition.promptGuidelines] }),
      ...(local?.definition.annotations === undefined ? {} : { annotations: { ...local.definition.annotations } }),
      ...(local?.definition.namespace === undefined ? {} : { namespace: { ...local.definition.namespace } }),
      ...(local ? { sourceInfo: { path: local.factory, source: 'extension', scope: 'temporary', origin: 'top-level' } } : {}),
    };
  });
}

export function setActiveTools(runtime, factory, names) {
  if (!Array.isArray(names) || names.length > 256) invalid('active tools');
  for (const name of names) bounded(name, 'active tool name', 128);
  runtime.require('active_tools');
  const store = runtime.current(factory);
  // Pi ignores unknown names. Do not optimistically claim the host applied a
  // requested set, and do not relax native product policy.
  const registered = new Set(toolSnapshot(runtime, factory).all_tools.map(tool => tool.name));
  runtime.transport.requestSync('tools/set_active', {
    parent_request_id: store.id, resource_owner: store.state.owner,
    names: [...new Set(names)].filter(name => registered.has(name)),
  }, { parent: store.live ? store.id : undefined, signal: store.controller.signal });
}

export async function toolContent(runtime, content, store) {
  if (!Array.isArray(content) || content.length > 64) invalid('tool content must be an array of at most 64 parts');
  const parts = [];
  for (const part of content) {
    if (part?.type === 'text') {
      fields(part, ['type', 'text'], 'tool text');
      parts.push({ type: 'text', text: bounded(part.text, 'tool output', 524288, { controls: true }) });
    } else if (part?.type === 'image') {
      fields(part, ['type', 'data', 'mimeType'], 'tool image'); runtime.require('artifacts');
      bounded(part.data, 'tool image data', 524288, { controls: true });
      if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(part.data)) invalid('tool image must be base64');
      if (!['image/png', 'image/jpeg', 'image/gif', 'image/webp'].includes(part.mimeType)) invalid('tool image mimeType');
      const bytes = Buffer.from(part.data, 'base64');
      const artifact = await runtime.hostCall('artifact/publish', {
        mime_type: part.mimeType, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'),
        data: { encoding: 'base64', data: part.data },
      }, store);
      // The host acknowledges with { artifact_id } (PROTOCOL-REFERENCE: artifact/publish).
      if (typeof artifact?.artifact_id !== 'string') invalid('artifact publication acknowledgement');
      parts.push({ type: 'image', artifact_id: artifact.artifact_id, mime_type: part.mimeType });
    } else unsupported('tool content type', 'Pi supports text and image content');
  }
  return parts;
}

// Native preparation is a separate host request before the exact schema gate.
export function prepareRegisteredArguments(runtime, params, store) {
  const entry = runtime.tools.get(params.name); if (!entry) invalid(`unknown tool ${params.name}`);
  store.factory = entry.factory;
  const value = runtime.scope.run(store, () => entry.definition.prepareArguments(params.arguments));
  return { arguments: plainJSON(value, 'prepared tool arguments', 131072) };
}

function nativePart(part) {
  if (typeof part.Text === 'string') return { type: 'text', text: part.Text };
  // Native ImageSource::Inline serializes as standard base64 (octet-ai base64_bytes).
  const image = part.Media?.Image, data = image?.source?.Inline;
  if (!image || typeof data !== 'string' || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(data) || typeof image.media_type !== 'string') unsupported('nested tool media', 'Pi requires verified inline image bytes and MIME type');
  return { type: 'image', data, mimeType: image.media_type };
}
function nativeResult(value) {
  fields(value, ['content', 'is_error', 'metadata', 'structured_content', 'usage'], 'nested tool result');
  if (!Array.isArray(value.content) || typeof value.is_error !== 'boolean') invalid('nested tool result');
  if (value.usage != null) unsupported('nested tool usage', 'native aggregate usage has no lossless Pi tool-usage binding');
  return { content: value.content.map(nativePart), details: value.metadata?.pi_details,
    ...(value.structured_content === undefined ? {} : { structuredContent: value.structured_content }), isError: value.is_error };
}
function nativeOutcome(value) {
  fields(value, ['tool_call', 'content', 'is_error', 'metadata', 'structured_content', 'usage'], 'nested tool outcome');
  fields(value.tool_call, ['id', 'name', 'arguments'], 'nested tool call');
  bounded(value.tool_call.id, 'nested tool call id', 256);
  bounded(value.tool_call.name, 'nested tool call name', 128);
  const { tool_call, ...result } = value;
  return { toolCall: tool_call, result: nativeResult(result), isError: value.is_error };
}

// Reuse the existing native composition sidecar contract, validating identity,
// exact bytes and digest before any JSON is consumed. No arbitrary host path.
async function compositionReply(runtime, reply, key) {
  const reference = reply[`${key}_file`];
  if (reference === undefined) return key === 'value' ? reply.value : reply;
  fields(reference, ['path', 'bytes', 'sha256'], 'composition sidecar');
  const directory = process.env.OCTET_EXTENSION_SCRATCH;
  if (!directory || !isAbsolute(directory) || !/^[A-Za-z0-9_.-]{1,256}$/.test(reference.path) || ['.', '..'].includes(reference.path)
    || !Number.isSafeInteger(reference.bytes) || reference.bytes < 1 || reference.bytes > 8 * 1024 * 1024 || !/^[a-f0-9]{64}$/.test(reference.sha256)) invalid('composition sidecar reference');
  const path = join(directory, reference.path); let handle;
  try {
    const before = await lstat(path); if (!before.isFile() || before.isSymbolicLink()) invalid('composition sidecar type');
    handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0));
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size !== reference.bytes || stat.dev !== before.dev || stat.ino !== before.ino) invalid('composition sidecar identity');
    const bytes = Buffer.alloc(reference.bytes + 1); let length = 0;
    while (length < bytes.length) { const read = await handle.read(bytes, length, bytes.length - length, length); if (!read.bytesRead) break; length += read.bytesRead; }
    if (length !== reference.bytes || (await handle.stat()).size !== reference.bytes || createHash('sha256').update(bytes.subarray(0, length)).digest('hex') !== reference.sha256) invalid('composition sidecar bytes');
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)));
  } finally { try { await handle?.close(); } finally { await unlink(path).catch(error => { if (error.code !== 'ENOENT') throw error; }); } }
}

export async function createToolContext(runtime, params, store, context) {
  store.factory = runtime.tools.get(params.name)?.factory;
  if (!runtime.features.has('tool_composition_v1')) return context;
  const executeTool = async (name, args, options = {}) => {
    bounded(name, 'nested tool name', 128); fields(options, ['signal', 'onUpdate'], 'executeTool options');
    if (options.onUpdate !== undefined && typeof options.onUpdate !== 'function') invalid('executeTool onUpdate');
    const controller = new AbortController(), signals = [store.controller.signal, options.signal].filter(Boolean);
    const abort = event => controller.abort(event.target.reason);
    for (const signal of signals) { if (!(signal instanceof AbortSignal)) invalid('executeTool signal'); if (signal.aborted) controller.abort(signal.reason); else signal.addEventListener('abort', abort, { once: true }); }
    try {
      const reply = await runtime.transport.request('composition/call', { parent_request_id: store.id, name, arguments: plainJSON(args, 'nested arguments', 131072), full_outcome: true,
        ...(options.onUpdate === undefined ? {} : { updates: true }) }, { parent: store.id, signal: controller.signal,
        ...(options.onUpdate === undefined ? {} : { onUpdate: value => runtime.scope.run(store, () => options.onUpdate(nativeResult(value))) }) });
      return nativeOutcome(await compositionReply(runtime, reply, 'value'));
    } finally { for (const signal of signals) signal.removeEventListener('abort', abort); }
  };
  const tools = () => {
    const reply = runtime.transport.requestSync('composition/context', { parent_request_id: store.id }, { parent: store.id, signal: store.controller.signal });
    let catalog = reply;
    if (reply.context_file !== undefined) {
      const reference = reply.context_file, directory = process.env.OCTET_EXTENSION_SCRATCH;
      fields(reference, ['path', 'bytes', 'sha256'], 'composition sidecar');
      if (!directory || !isAbsolute(directory) || !/^[A-Za-z0-9_.-]{1,256}$/.test(reference.path) || ['.', '..'].includes(reference.path)
        || !Number.isSafeInteger(reference.bytes) || reference.bytes < 1 || reference.bytes > 8 * 1024 * 1024 || !/^[a-f0-9]{64}$/.test(reference.sha256)) invalid('composition sidecar reference');
      const path = join(directory, reference.path), before = lstatSync(path); let fd;
      if (!before.isFile() || before.isSymbolicLink()) invalid('composition sidecar type');
      try {
        fd = openSync(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0));
        const stat = fstatSync(fd);
        if (!stat.isFile() || stat.size !== reference.bytes || stat.dev !== before.dev || stat.ino !== before.ino) invalid('composition sidecar identity');
        const bytes = Buffer.alloc(reference.bytes + 1); let length = 0;
        while (length < bytes.length) { const read = readSync(fd, bytes, length, bytes.length - length, length); if (!read) break; length += read; }
        if (length !== reference.bytes || fstatSync(fd).size !== reference.bytes || createHash('sha256').update(bytes.subarray(0, length)).digest('hex') !== reference.sha256) invalid('composition sidecar bytes');
        catalog = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)));
      } finally { if (fd !== undefined) closeSync(fd); unlinkSync(path); }
    }
    if (!Array.isArray(catalog.tools)) invalid('nested tool catalog');
    return catalog.tools.map(tool => ({ name: tool.name, label: runtime.tools.get(tool.name)?.definition.label ?? tool.name,
      description: tool.description, parameters: tool.parameters, ...(tool.output_schema === undefined ? {} : { outputSchema: tool.output_schema }),
      execute: async (_id, args, signal, onUpdate) => (await executeTool(tool.name, args, { signal, ...(onUpdate === undefined ? {} : { onUpdate }) })).result }));
  };
  Object.defineProperties(context, { tools: { get: tools, enumerable: true }, executeTool: { value: executeTool, enumerable: true } });
  return context;
}

export function prepareToolLoadout(runtime, store, declared) {
  if (!Array.isArray(declared)) invalid('declared tool loadout');
  const callbacks = declared.flatMap(tool => {
    const entry = runtime.tools.get(tool.name); return entry?.definition.prepareLoadout ? [entry] : [];
  });
  if (!callbacks.length) return undefined;
  const snapshot = toolSnapshot(runtime, store.factory), all = getAllTools(runtime, store.factory, snapshot), active = new Set(snapshot.active_tools);
  const signature = JSON.stringify([declared, all, snapshot.active_tools]);
  const previous = store.state.piToolLoadout;
  if (previous?.signature === signature && previous.callbacks.every((entry, index) => entry === callbacks[index]) && previous.callbacks.length === callbacks.length) return previous.projection;
  const descriptor = tool => ({ ...tool, label: runtime.tools.get(tool.name)?.definition.label ?? tool.name });
  const loadout = { declared: declared.map(descriptor), callable: all.filter(tool => active.has(tool.name) && tool.exposure !== 'model-only' && tool.exposure !== 'hidden').map(descriptor),
    registered: all.map(descriptor), getExposure: name => all.find(tool => tool.name === name)?.exposure ?? 'direct',
    getNamespace: name => runtime.tools.get(name)?.definition.namespace };
  const descriptions = new Map(), hidden = new Set();
  for (const entry of callbacks) {
    try {
      const changes = runtime.scope.run({ ...store, factory: entry.factory }, () => entry.definition.prepareLoadout(loadout));
      if (changes === undefined) continue;
      fields(changes, ['descriptions', 'hiddenDeclarations'], 'prepareLoadout result');
      if (changes.descriptions !== undefined) {
        const clean = plainJSON(changes.descriptions, 'loadout descriptions', 1048576);
        if (!clean || Array.isArray(clean) || typeof clean !== 'object') invalid('loadout descriptions');
        for (const [name, description] of Object.entries(clean)) descriptions.set(name, bounded(description, 'loadout description', 65536, { controls: true }));
      }
      if (changes.hiddenDeclarations !== undefined) {
        if (!Array.isArray(changes.hiddenDeclarations) || changes.hiddenDeclarations.length > 256) invalid('hidden declarations');
        for (const name of changes.hiddenDeclarations) hidden.add(bounded(name, 'hidden declaration', 128));
      }
    } catch (error) { runtime.backgroundError(error); }
  }
  const projection = declared.filter(tool => !hidden.has(tool.name)).map(tool => descriptions.has(tool.name) ? { ...tool, description: descriptions.get(tool.name) } : tool);
  store.state.piToolLoadout = { signature, callbacks, projection };
  return projection;
}

export async function executeRegisteredTool(runtime, params, store, context) {
  const tool = runtime.tools.get(params.name); if (!tool) invalid(`unknown tool ${params.name}`);
  store.factory = tool.factory; let sequence = 0, settled = false, queued = 0, tail = Promise.resolve();
  const update = runtime.features.has('request_progress') ? result => {
    if (settled) return; // Pi ignores callbacks made after execute settles.
    fields(result, ['content', 'details', 'structuredContent', 'isError'], 'tool update');
    store.controller.signal.throwIfAborted();
    if (queued >= 64) invalid('tool update queue exceeds 64 pending results');
    // Snapshot immediately, then serialize artifact publication and delivery so
    // a slower image update cannot overtake a later text/details update.
    const snapshot = plainJSON(Object.fromEntries(Object.entries(result).filter(([, value]) => value !== undefined)), 'tool update', 1048576);
    if (snapshot.isError !== undefined && typeof snapshot.isError !== 'boolean') invalid('tool update isError must be boolean');
    queued++;
    const pending = tail.then(async () => {
      store.controller.signal.throwIfAborted();
      const content = await toolContent(runtime, snapshot.content, store);
      return runtime.transport.notify('$/progress', { request_id: store.id, sequence: ++sequence,
        event: { type: 'partial_result', result: { content, is_error: snapshot.isError ?? false,
          ...(snapshot.details === undefined ? {} : { metadata: { pi_details: plainJSON(snapshot.details, 'tool update details') } }),
          ...(snapshot.structuredContent === undefined ? {} : { structured_content: plainJSON(snapshot.structuredContent, 'tool update structured content', 262144) }) } } });
    });
    tail = pending;
    pending.then(() => { queued--; }, () => { queued--; });
    runtime.track(pending, store);
  } : undefined;
  let result;
  try { result = await runtime.scope.run(store, () => tool.definition.execute(String(store.id), params.arguments, store.controller.signal, update, context)); }
  finally { settled = true; }
  fields(result, ['content', 'details', 'isError', 'structuredContent'], 'tool result');
  if (result.isError !== undefined && typeof result.isError !== 'boolean') invalid('tool isError must be boolean');
  const content = await toolContent(runtime, result.content, store);
  await runtime.flush(store);
  return { content, is_error: result.isError ?? false,
    ...(result.details === undefined ? {} : { metadata: { pi_details: plainJSON(result.details, 'tool details') } }),
    ...(result.structuredContent === undefined ? {} : { structured_content: plainJSON(result.structuredContent, 'structured content', 262144) }),
  };
}

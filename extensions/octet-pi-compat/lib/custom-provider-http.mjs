import { bounded, fields, invalid, plainJSON, rpcError, unsupported } from './errors.mjs';

function headerObject(value, label) {
  if (value === undefined) return {};
  const headers = plainJSON(value, label, 32768);
  if (!headers || Array.isArray(headers) || typeof headers !== 'object') invalid(label);
  for (const [name, text] of Object.entries(headers)) {
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name) || typeof text !== 'string' || /[\r\n\0]/.test(text)) invalid(label);
  }
  return headers;
}
function appendHeaders(target, source) {
  for (const [name, value] of Object.entries(source)) {
    for (const previous of Object.keys(target)) if (previous.toLowerCase() === name.toLowerCase()) delete target[previous];
    target[name] = value;
  }
}
export function prepareCustomProviderHttp(job, model, options) {
  const fetch = options.fetch ?? globalThis.fetch;
  if (typeof fetch !== 'function') invalid('custom provider fetch');
  const headers = {};
  if (options.apiKey !== undefined) {
    bounded(options.apiKey, 'custom provider key', 65536, { controls: true });
    if (/[\r\n\0]/.test(options.apiKey)) invalid('custom provider key');
    headers.Authorization = `Bearer ${options.apiKey}`;
  }
  appendHeaders(headers, headerObject(model.headers, 'custom model headers'));
  appendHeaders(headers, headerObject(options.headers, 'custom request headers'));
  headerObject(headers, 'custom combined headers');
  const controller = new AbortController();
  const cancel = () => {
    controller.abort();
    Promise.resolve(job.customReader?.cancel()).catch(() => {});
  };
  job.controller.signal.addEventListener('abort', cancel, { once: true });
  job.customFetch = fetch;
  job.customHttp = { model: structuredClone(model), headers, controller, cancel, signal: AbortSignal.any([job.controller.signal, controller.signal]), opened: false, pulling: false, pending: undefined, done: false };
}
export function closeCustomProviderHttp(job) {
  if (!job?.customHttp) return;
  job.controller.signal.removeEventListener('abort', job.customHttp.cancel);
  job.customHttp.cancel();
}
async function operation(job, work) {
  const state = job.customHttp;
  state.signal.throwIfAborted();
  let timer, abort;
  const stopped = new Promise((_, reject) => {
    abort = () => reject(rpcError(-32800, 'custom provider HTTP cancelled'));
    state.signal.addEventListener('abort', abort, { once: true });
    timer = setTimeout(() => { reject(rpcError(-32002, 'custom provider HTTP timeout')); state.cancel(); }, 5000);
  });
  try {
    const result = await Promise.race([Promise.resolve().then(work), stopped]);
    state.signal.throwIfAborted();
    return result;
  } finally {
    clearTimeout(timer);
    state.signal.removeEventListener('abort', abort);
  }
}
function active(job) {
  if (!job?.delegated || !job.entry.custom || job.entry.inherited || !job.customHttp || typeof job.customFetch !== 'function') unsupported('custom provider HTTP', 'a live admitted custom delegate is required');
  job.controller.signal.throwIfAborted();
  job.customHttp.signal.throwIfAborted();
  return job.customHttp;
}
export async function openCustomProviderHttp(job, params) {
  const state = active(job);
  fields(params, ['stream_id', 'method', 'url', 'headers', 'body'], 'custom provider HTTP request');
  if (state.opened) unsupported('custom provider HTTP', 'only one fetch is permitted per delegate');
  if (params.method !== 'POST') invalid('custom provider HTTP method');
  bounded(params.url, 'custom provider HTTP URL', 8192);
  const expected = `${state.model.baseUrl.replace(/\/$/, '')}/${state.model.api === 'openai-completions' ? 'chat/completions' : 'responses'}`;
  if (params.url !== expected) unsupported('custom provider HTTP URL', 'the selected custom endpoint cannot change');
  const url = new URL(params.url);
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) invalid('custom provider HTTP URL');
  bounded(params.body, 'custom provider HTTP body', 786432, { controls: true });
  let body;
  try { body = JSON.parse(params.body); } catch { invalid('custom provider HTTP JSON body'); }
  if (!body || body.model !== state.model.id || body.stream !== true) unsupported('custom provider HTTP body', 'model and stream authority cannot change');
  const headers = headerObject(params.headers, 'custom host headers');
  if (Object.keys(headers).some(name => /^(authorization|proxy-authorization|cookie|x-api-key)$/i.test(name))) unsupported('custom host headers', 'host credentials cannot enter custom transport');
  appendHeaders(headers, state.headers);
  headerObject(headers, 'custom HTTP headers');
  state.opened = true;
  try {
    return await operation(job, async () => {
      const response = await job.customFetch(params.url, { method: 'POST', headers, body: params.body, signal: state.signal });
      state.signal.throwIfAborted();
      if (!Number.isInteger(response?.status) || response.status < 100 || response.status > 599 || typeof response.headers?.entries !== 'function') invalid('custom provider HTTP response');
      const responseHeaders = headerObject(Object.fromEntries(response.headers.entries()), 'custom response headers');
      if (response.body === null) state.done = true;
      else {
        if (typeof response.body?.getReader !== 'function') invalid('custom provider HTTP response body');
        job.customReader = response.body.getReader();
      }
      return { status: response.status, headers: responseHeaders };
    });
  } catch (error) { state.cancel(); throw error; }
}
export async function pullCustomProviderHttp(job, params) {
  const state = active(job);
  fields(params, ['stream_id'], 'custom provider HTTP pull');
  if (!state.opened || state.pulling) unsupported('custom provider HTTP pull', 'one opened response and one reader are required');
  if (state.done) return { done: true };
  state.pulling = true;
  try {
    return await operation(job, async () => {
      let bytes = state.pending;
      if (!bytes) {
        const next = await job.customReader.read();
        state.signal.throwIfAborted();
        if (next.done) { state.done = true; return { done: true }; }
        if (!(next.value instanceof Uint8Array)) invalid('custom provider HTTP response bytes');
        bytes = next.value;
      }
      const chunk = bytes.subarray(0, 65536);
      state.pending = bytes.length > chunk.length ? bytes.subarray(chunk.length) : undefined;
      return { chunk: Buffer.from(chunk).toString('base64') };
    });
  } catch (error) { state.cancel(); throw error; }
  finally { state.pulling = false; }
}

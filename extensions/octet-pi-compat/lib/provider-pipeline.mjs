// Actual encoded provider phases only; never substitute a canonical Request.
import { bounded, fields, invalid, ownerKey, plainJSON, rpcError, strict, unsupported } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';
export const pipelineHooks = ['before_provider_request', 'before_provider_headers', 'after_provider_response'];
function headers(value, deleting = false) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).length > 256) invalid('provider header map');
  const result = Object.create(null);
  for (const [key, entry] of Object.entries(value)) {
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,128}$/.test(key)) invalid('provider header name');
    const name = key.toLowerCase(); if (Object.hasOwn(result, name)) invalid('duplicate provider header name');
    if (entry === null && deleting) { result[name] = null; continue; }
    const values = Array.isArray(entry) ? entry : [entry];
    if (!values.length || values.length > 128) invalid('provider header values');
    for (const item of values) { bounded(item, 'provider header value', 16384, { controls: true }); if (/[\x00-\x08\x0a-\x1f\x7f]/u.test(item)) invalid('provider header controls'); }
    result[name] = Array.isArray(entry) ? [...values] : entry;
  }
  return plainJSON(result, 'provider headers', 65536);
}
export async function providerPipeline(runtime, params, store) {
  runtime.require('pipeline_hooks_v1');
  const hook = params.hook;
  if (!pipelineHooks.includes(hook) || !runtime.metadata().hooks.includes(hook)) invalid('undeclared provider pipeline hook');
  ownerKey(params.context?.resource_owner); runtime.bind(params, store); store.providerContext = true;
  const body = params.payload;
  fields(body, hook === 'before_provider_request' ? ['operation_id', 'model', 'payload'] : hook === 'before_provider_headers' ? ['operation_id', 'model', 'headers'] : ['operation_id', 'model', 'status', 'headers'], 'provider pipeline');
  bounded(body.operation_id, 'provider operation', 256);
  if (!body.operation_id) invalid('provider operation');
  fields(body.model, ['id', 'provider', 'api'], 'provider model');
  for (const value of Object.values(body.model)) bounded(value, 'provider model identity', 256);
  const signal = store.controller.signal, live = () => { signal.throwIfAborted(); runtime.assertOwner(store); };
  return cancellable(runtime.queued(store, async () => {
    live();
    let payload = hook === 'before_provider_request' ? plainJSON(body.payload, 'provider payload', 786432) : undefined;
    const originalHeaders = hook !== 'before_provider_request' ? headers(body.headers) : undefined;
    const mutableHeaders = originalHeaders && structuredClone(originalHeaders);
    if (hook === 'after_provider_response' && (!Number.isInteger(body.status) || body.status < 100 || body.status > 599)) invalid('provider response status');
    for (const entry of [...runtime.events.get(hook) || []]) {
      live(); const child = { ...store, factory: entry.factory };
      const event = hook === 'before_provider_request' ? { type: hook, payload }
        : hook === 'before_provider_headers' ? { type: hook, headers: mutableHeaders }
        : { type: hook, status: body.status, headers: Object.fromEntries(Object.entries(originalHeaders).map(([key, value]) => [key, Array.isArray(value) ? value.join(', ') : value])) };
      let result;
      try { result = await cancellable(runtime.scope.run(child, () => entry.handler(strict(event, `${hook} event`), createContext(runtime, child))), signal); }
      catch (error) {
        live();
        // These callbacks see private wire material. Never echo their exception
        // text, JSON payloads or headers to diagnostic/UI/session channels.
        if (Number.isInteger(error?.code)) throw rpcError(error.code, 'provider pipeline callback refused');
        runtime.backgroundError(new Error(`${hook} factory ${entry.factory} failed`)); continue;
      }
      live();
      if (hook === 'before_provider_request' && result !== undefined) payload = plainJSON(result, 'provider payload', 786432);
      // Pi header/response callback return values have no semantic effect;
      // headers are changed in place, and response arrival is observation only.
    }
    await cancellable(runtime.flush(store), signal); live();
    if (hook === 'before_provider_request') {
      if (!payload || typeof payload !== 'object') invalid('provider payload replacement must be JSON object/array');
      return { provider_payload: payload };
    }
    if (hook === 'before_provider_headers') {
      const final = headers(mutableHeaders, true), patch = {};
      for (const key of new Set([...Object.keys(originalHeaders), ...Object.keys(final)])) {
        if (!Object.hasOwn(final, key)) patch[key] = null;
        else if (JSON.stringify(originalHeaders[key]) !== JSON.stringify(final[key])) patch[key] = final[key];
      }
      return { provider_headers: patch };
    }
    return {};
  }), signal);
}

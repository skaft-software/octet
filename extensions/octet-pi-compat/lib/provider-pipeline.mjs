// Actual encoded provider phases only; never substitute a canonical Request.
import { bounded, facade, fields, invalid, ownerKey, plainJSON, rpcError, strict, unsupported } from './errors.mjs';
import { createContext } from './api.mjs';
import { cancellable } from './provider-context.mjs';
import { HookBudget } from './handler-budget.mjs';
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
// Pi passes handler-mutated headers to fetch, which stringifies numbers and
// booleans. Mirror that, and drop (with one report) any entry fetch would
// reject, instead of rejecting the whole request.
function normalizeHeaders(value, reportDropped) {
  const result = Object.create(null);
  let dropped = false;
  for (const [key, entry] of Object.entries(value && typeof value === 'object' ? value : {})) {
    if (entry === undefined) continue;
    if (entry === null) { result[key.toLowerCase()] = null; continue; }
    const values = (Array.isArray(entry) ? entry : [entry]).map(item => typeof item === 'number' || typeof item === 'boolean' || typeof item === 'bigint' ? String(item) : item);
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,128}$/.test(key) || !values.length || values.length > 128
      || values.some(item => typeof item !== 'string' || item.length > 16384 || /[\x00-\x08\x0a-\x1f\x7f]/u.test(item))) { dropped = true; continue; }
    // Header names are case-insensitive; the latest assignment wins, as when
    // a handler sets `Authorization` over an existing `authorization`.
    result[key.toLowerCase()] = Array.isArray(entry) ? values : values[0];
  }
  if (dropped) reportDropped();
  return result;
}
// Pi sends JSON.stringify(payload): undefined members disappear and Dates
// become strings. Normalize the same way; a non-object replacement or one that
// cannot be encoded keeps the previous payload (Pi would fail at send time).
function normalizePayload(value) {
  let encoded;
  try { encoded = JSON.stringify(value); } catch { return undefined; }
  if (encoded === undefined) return undefined;
  const parsed = JSON.parse(encoded);
  return parsed && typeof parsed === 'object' ? parsed : undefined;
}
export async function providerPipeline(runtime, params, store) {
  runtime.require('pipeline_hooks_v1');
  const hook = params.hook;
  if (!pipelineHooks.includes(hook) || !runtime.metadata().hooks.includes(hook)) invalid('undeclared provider pipeline hook');
  ownerKey(params.context?.resource_owner);
  const body = params.payload;
  fields(body, hook === 'before_provider_request' ? ['operation_id', 'model', 'payload'] : hook === 'before_provider_headers' ? ['operation_id', 'model', 'headers'] : ['operation_id', 'model', 'status', 'headers'], 'provider pipeline');
  bounded(body.operation_id, 'provider operation', 256);
  if (!body.operation_id) invalid('provider operation');
  fields(body.model, ['id', 'provider', 'api'], 'provider model');
  for (const value of Object.values(body.model)) bounded(value, 'provider model identity', 256);
  runtime.bind(params, store); store.providerContext = true;
  const signal = store.controller.signal, live = () => { signal.throwIfAborted(); runtime.assertSessionOwner(store); };
  return cancellable(runtime.queued(store, async () => {
    live();
    let payload = hook === 'before_provider_request' ? plainJSON(body.payload, 'provider payload', 786432) : undefined;
    const originalHeaders = hook !== 'before_provider_request' ? headers(body.headers) : undefined;
    const mutableHeaders = originalHeaders && structuredClone(originalHeaders);
    if (hook === 'after_provider_response' && (!Number.isInteger(body.status) || body.status < 100 || body.status > 599)) invalid('provider response status');
    const budget = new HookBudget(runtime, store, hook);
    for (const entry of [...runtime.events.get(hook) || []]) {
      live(); const child = { ...store, factory: entry.factory };
      const event = hook === 'before_provider_request' ? { type: hook, payload }
        : hook === 'before_provider_headers' ? { type: hook, headers: mutableHeaders }
        : { type: hook, status: body.status, headers: Object.fromEntries(Object.entries(originalHeaders).map(([key, value]) => [key, Array.isArray(value) ? value.join(', ') : value])) };
      let result;
      try {
        const run = await cancellable(budget.run(entry, child, () => runtime.scope.run(child, () => entry.handler(facade(event, `${hook} event`), createContext(runtime, child)))), signal);
        if (!run.ran) continue; // a stalled provider callback must not hold the model request
        result = run.value;
      }
      catch (error) {
        live();
        // These callbacks see private wire material. Never echo their exception
        // text, JSON payloads or headers to diagnostic/UI/session channels.
        // A host refusal reached through an octet call (a JSON-RPC error code)
        // propagates. Anything else is the handler's own failure: Pi reports
        // it and continues with the current value (runner.ts
        // emitBeforeProvider*). A DOMException's small positive `code`, as from
        // an aborted fetch, is not a host refusal.
        if (Number.isInteger(error?.code) && error.code <= -32000 && error.code >= -32899) throw rpcError(error.code, 'provider pipeline callback refused');
        runtime.reportCallbackError(hook, entry.factory, error); continue;
      }
      live();
      if (hook === 'before_provider_request' && result !== undefined) {
        const next = normalizePayload(result);
        if (next === undefined) runtime.reportCallbackError(hook, entry.factory, new TypeError('provider payload replacement must be a JSON object'));
        else payload = next;
      }
      // Pi header/response callback return values have no semantic effect;
      // headers are changed in place, and response arrival is observation only.
    }
    budget.finish();
    await cancellable(runtime.flush(store), signal); live();
    if (hook === 'before_provider_request') {
      return { provider_payload: plainJSON(payload, 'provider payload', 786432) };
    }
    if (hook === 'before_provider_headers') {
      const final = headers(normalizeHeaders(mutableHeaders, () => runtime.reportCallbackError(hook, 'provider headers', new TypeError('a header value could not be sent and was dropped'))), true), patch = {};
      for (const key of new Set([...Object.keys(originalHeaders), ...Object.keys(final)])) {
        if (!Object.hasOwn(final, key)) patch[key] = null;
        else if (JSON.stringify(originalHeaders[key]) !== JSON.stringify(final[key])) patch[key] = final[key];
      }
      return { provider_headers: patch };
    }
    return {};
  }), signal);
}

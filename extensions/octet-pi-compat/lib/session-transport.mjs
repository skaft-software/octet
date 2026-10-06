import { jsonBytes } from './json-count.mjs';
import { appendFileSync } from 'node:fs';
const dbg = value => { try { appendFileSync('/tmp/u1-adapter-debug.log', `${value}\n`); } catch {} };
// session-repair.v2. Tokens identify inert documents, never session authority.
import { createHash } from 'node:crypto';
import { fields, invalid, ownerKey, rpcError } from './errors.mjs';

export const SESSION_FEATURES = ['session_owner_routes_v1', 'session_snapshot_transport_v1'];
const profileKeys = ['profile', 'chunk_bytes', 'snapshot_bytes', 'generation_bytes', 'owner_views', 'transfers', 'view_entries', 'generation_entries', 'projection_bytes', 'projections'];
const descriptorKeys = ['transfer_id', 'kind', 'owner', 'view_revision', 'head', 'bytes', 'sha256', 'entry_count', 'branch_count', 'preparation'];
const hash = value => createHash('sha256').update(value).digest('hex');
const hex = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const integer = (value, minimum = 0) => Number.isSafeInteger(value) && value >= minimum;
function exact(value, keys, label) {
  fields(value, keys, label);
  if (Object.keys(value).length !== keys.length) invalid(`${label} missing fields`);
}
const unavailable = () => rpcError(-32002, 'session view unavailable');
function descriptor(value, owner, kind) {
  exact(value, descriptorKeys, 'session descriptor');
  exact(value.owner, ['session_id', 'extension_instance_id', 'process_generation'], 'session descriptor owner');
  if (ownerKey(value.owner) !== ownerKey(owner) || !integer(value.owner.process_generation, 1)
      || !hex(value.transfer_id) || !hex(value.sha256) || value.kind !== kind || !integer(value.view_revision, 1)
      || !integer(value.bytes, 1) || !integer(value.entry_count) || !integer(value.branch_count)
      || !(value.head === null || typeof value.head === 'string')) invalid('session descriptor identity');
  if (value.preparation !== null) {
    exact(value.preparation, ['activation_epoch', 'operation_id', 'tool_generation', 'head'], 'session preparation');
    if (!integer(value.preparation.activation_epoch, 1) || typeof value.preparation.operation_id !== 'string'
        || !integer(value.preparation.tool_generation) || value.preparation.head !== value.head) invalid('session preparation identity');
  }
  return value;
}

export class SessionTransport {
  constructor(runtime, profile, frame) {
    exact(profile, profileKeys, 'session transport profile');
    if (profile.profile !== 'json-chunks.v1' || profileKeys.slice(1).some(key => !integer(profile[key], 1))
        || profile.chunk_bytes > 65536 || !integer(frame, 1)) invalid('session transport profile');
    this.runtime = runtime; this.profile = Object.freeze({ ...profile }); this.frame = frame;
    this.bytes = 0; this.records = 0; this.views = 0; this.transfers = 0; this.projectionBytes = 0; this.projections = 0;
    this.current = new Map(); this.activations = new Map();
  }
  reserve(d) {
    const p = this.profile, records = d.entry_count + d.branch_count;
    if (d.bytes > p.snapshot_bytes || d.entry_count > p.view_entries || d.branch_count > p.view_entries
        || this.bytes + d.bytes > p.generation_bytes || this.records + records > p.generation_entries || this.views >= p.owner_views) throw unavailable();
    this.bytes += d.bytes; this.records += records; this.views++;
    let released = false;
    return () => { if (released) return; released = true; this.bytes -= d.bytes; this.records -= records; this.views--; };
  }
  call(store, method, params) {
    store.controller.signal.throwIfAborted();
    if (!store.live || this.runtime.active.get(store.id)?.controller !== store.controller) throw unavailable();
    return this.runtime.transport.request(method, { parent_request_id: store.id, ...params }, { parent: store.id, signal: store.controller.signal });
  }
  async read(d, store) {
    if (this.transfers >= this.profile.transfers) throw unavailable();
    const release = this.reserve(d); this.transfers++;
    let bytes;
    try {
      // Reserve the exact announced bytes/records BEFORE allocating any buffer.
      bytes = Buffer.allocUnsafe(d.bytes);
      let offset = 0;
      while (offset < d.bytes) {
        const chunk = await this.call(store, 'session/snapshot/read', { transfer_id: d.transfer_id, offset, max_bytes: this.profile.chunk_bytes });
        exact(chunk, ['transfer_id', 'offset', 'data', 'next_offset', 'eof'], 'session chunk');
        if (chunk.transfer_id !== d.transfer_id || chunk.offset !== offset || typeof chunk.data !== 'string'
            || chunk.data.length > Math.ceil(this.profile.chunk_bytes / 3) * 4) invalid('session chunk identity');
        const data = Buffer.from(chunk.data, 'base64');
        if (!data.length || data.length > this.profile.chunk_bytes || data.toString('base64') !== chunk.data
            || chunk.next_offset !== offset + data.length || chunk.next_offset > d.bytes || chunk.eof !== (chunk.next_offset === d.bytes)) invalid('session chunk encoding or length');
        data.copy(bytes, offset); offset = chunk.next_offset;
      }
      if (hash(bytes) !== d.sha256) invalid('session document digest');
      const document = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
      const result = await this.call(store, 'session/snapshot/release', { transfer_id: d.transfer_id });
      exact(result, ['released'], 'session release'); if (result.released !== true) invalid('session release');
      // Drop the encoded representation; the same reservation follows its parsed
      // view until current and all immutable invocation pins release it.
      bytes = undefined;
      return { descriptor: d, document, release, refs: 1 };
    } catch (error) { dbg(`read failed transfer=${d.transfer_id.slice(0, 8)}: ${error?.message}`); bytes = undefined; release(); throw error; }
    finally { this.transfers--; }
  }
  retain(view) { view.refs++; return view; }
  drop(view) { if (view && --view.refs === 0) { view.document = undefined; view.host = undefined; view.release(); } }
  history(view) {
    const d = view.descriptor, h = view.document;
    exact(h, ['entries', 'branch_ids', 'head', 'file', 'header', 'labels'], 'session history');
    if (!Array.isArray(h.entries) || h.entries.length !== d.entry_count || !Array.isArray(h.branch_ids) || h.branch_ids.length !== d.branch_count
        || h.head !== d.head || !(h.file === null || typeof h.file === 'string') || !h.header || typeof h.header !== 'object' || Array.isArray(h.header)
        || !h.labels || typeof h.labels !== 'object' || Array.isArray(h.labels) || Object.values(h.labels).some(v => typeof v !== 'string')) invalid('session history schema');
    const byId = new Map();
    for (const entry of h.entries) {
      if (!entry || typeof entry !== 'object' || Array.isArray(entry) || typeof entry.id !== 'string' || byId.has(entry.id)
          || !(entry.parent === null || typeof entry.parent === 'string') || !entry.value) invalid('session history entry');
      byId.set(entry.id, entry);
      // A foreign namespace must already have been filtered by the native host.
      for (const [namespace, metadata] of Object.entries(entry.metadata?.extension_metadata ?? {})) if (namespace !== this.runtime.namespace && metadata.public !== true) invalid('foreign private session metadata');
    }
    let previous = null;
    const branch = h.branch_ids.map(id => { const entry = byId.get(id); if (!entry || entry.parent !== previous) invalid('session history ancestry'); previous = id; return entry; });
    if (previous !== h.head) invalid('session history head');
    view.host = { session_entries: h.entries, session_branch: branch, session_leaf_id: h.head, session_file: h.file,
      session_header: Object.keys(h.header).length ? h.header : null, session_labels: h.labels };
    return view;
  }
  invalidate(owner, revision) {
    const key = ownerKey(owner), current = this.current.get(key);
    dbg(`invalidate rev=${revision} current=${current?.revision} replaced=${Boolean(current) && revision > current.revision}`);
    if (current && revision <= current.revision) return current;
    if (current) { this.drop(current.view); current.resolve?.(); }
    let resolve;
    const promise = new Promise(r => { resolve = r; });
    const next = { revision, view: undefined, promise, resolve };
    this.current.set(key, next);
    return next;
  }
  async prepare(params, store) {
    dbg(`prepare start rev=${params.snapshot?.view_revision}`);
    exact(params, ['resource_owner', 'snapshot', 'host'], 'session snapshot prepare');
    const d = descriptor(params.snapshot, params.resource_owner, 'history');
    if (d.preparation !== null) invalid('read-only snapshot preparation');
    this.runtime.bind({ context: { resource_owner: params.resource_owner, host: params.host } }, store);
    const key = ownerKey(d.owner), previous = this.current.get(key);
    if (previous && previous.revision > d.view_revision) throw unavailable();
    const barrier = this.invalidate(d.owner, d.view_revision);
    store.preparationBarrier = barrier; store.preparationOwner = key;
    let view;
    const hydrate = async () => {
      view = await this.read(d, store); this.history(view);
      store.controller.signal.throwIfAborted();
      if (this.current.get(key) !== barrier || !store.state.alive) { dbg(`hydrate superseded rev=${d.view_revision} same_barrier=${this.current.get(key) === barrier} alive=${store.state.alive}`); throw unavailable(); }
      this.drop(barrier.view); barrier.view = view; view = undefined;
    };
    try { await hydrate(); }
    catch (error) { dbg(`prepare failed revision=${d.view_revision} owner=${d.owner.session_id}: ${error?.message}`); this.drop(view); throw error; }
    finally { barrier.resolve?.(); if (this.current.get(key) === barrier) barrier.promise = undefined; }
    for (const surface of this.runtime.ui.surfaces.values()) if (surface.store.state === store.state) surface.requestRender();
    return { accepted: true, transfer_id: d.transfer_id, view_revision: d.view_revision, head: d.head, sha256: d.sha256 };
  }
  async hydrate(params, store) {
    if (params.session_snapshot === undefined) return;
    const owner = params.context?.resource_owner, d = descriptor(params.session_snapshot, owner, 'history');
    const view = await this.read(d, store);
    store.sessionView = view;
    this.history(view);
    if (params.session_payload !== undefined) {
      if (Object.hasOwn(params, 'payload')) invalid('inline and transferred invocation payload');
      const payload = descriptor(params.session_payload, owner, 'invocation');
      if (payload.entry_count || payload.branch_count || payload.view_revision !== d.view_revision || payload.head !== d.head
          || JSON.stringify(payload.preparation) !== JSON.stringify(d.preparation) || payload.bytes > this.profile.projection_bytes) invalid('invocation descriptor identity');
      const invocation = await this.read(payload, store);
      store.sessionPayload = invocation;
      params.payload = invocation.document;
    }
    if (d.preparation !== null) {
      const grant = params.session_leaf;
      if (!grant || grant.activation_epoch !== d.preparation.activation_epoch || grant.operation_id !== d.preparation.operation_id
          || grant.expected_head !== d.head || ownerKey(grant.owner) !== ownerKey(owner)) invalid('invocation preparation grant');
      if (params.hook === 'provider_context' && (params.payload?.preparation?.head !== d.head || params.payload?.preparation?.tool_generation !== d.preparation.tool_generation)) invalid('provider preparation identity');
      const key = JSON.stringify([ownerKey(owner), grant.activation_epoch, grant.operation_id]);
      let activation = this.activations.get(key);
      if (!activation) { activation = { key, view: this.retain(view), users: 0, leaf: { grant } }; this.activations.set(key, activation); }
      else {
        if (d.view_revision <= activation.view.descriptor.view_revision) invalid('stale linked invocation view');
        this.drop(activation.view); activation.view = this.retain(view); activation.leaf.grant = grant;
      }
      activation.users++; store.activation = activation;

    }
    // Invocation hydration is NOT a foreground/current publication.

  }
  // An invocation view belongs to the owner it was hydrated for; a replacement
  // store must never inherit the previous session's facts from its parent.
  ownView(store) {
    const view = store.activation?.view ?? store.sessionView;
    // Child contexts retain the object, not a transport lease. Once the last
    // lease settles, retained UI callbacks must use the published owner view.
    return view?.document !== undefined && ownerKey(view.descriptor.owner) === store.state.key ? view : undefined;
  }
  // The newest-wins revision decides between the published chunked view and an
  // admitted receipt's inline facts; equal revisions are the same document.
  publishedView(store) {
    // A concurrent owner publication cannot invalidate an invocation's pinned
    // history while its native parent (including a pending UI mount) is live.
    const own = this.ownView(store);
    if (own) return own;
    const view = this.current.get(store.state.key)?.view;
    const revision = store.state.host.session_view_revision;
    if (view && (!Number.isSafeInteger(revision) || view.descriptor.view_revision >= revision)) return view;
    return undefined;
  }
  host(store) {
    const view = this.publishedView(store);
    if (view) return { ...store.state.host, ...view.host };
    // A setup/replacement receipt may carry fresher facts inline; only an
    // admitted receipt's revision makes them readable.
    if (Number.isSafeInteger(store.state.host.session_view_revision) && Array.isArray(store.state.host.session_entries)) return { ...store.state.host };
    throw unavailable();
  }
  async ready(store) {
    if (this.publishedView(store)) return;
    // Superseding publication while awaiting an earlier barrier is not readiness.
    for (;;) {
      const current = this.current.get(store.state.key);
      if (!current) throw unavailable();
      if (current.promise) await current.promise;
      if (this.current.get(store.state.key) !== current) continue;
      if (!current.view) throw unavailable();
      return;
    }
  }
  settle(store, error) {
    if (error && store.preparationBarrier && this.current.get(store.preparationOwner) === store.preparationBarrier) {
      this.drop(store.preparationBarrier.view); store.preparationBarrier.view = undefined;
    }
    if (store.activation && --store.activation.users === 0) { this.activations.delete(store.activation.key); this.drop(store.activation.view); }
    this.drop(store.sessionView); this.drop(store.sessionPayload); store.sessionView = undefined; store.sessionPayload = undefined;
  }
  retire(state) { const current = this.current.get(state.key); this.current.delete(state.key); current?.resolve?.(); this.drop(current?.view); }
  append(store, receipt, grant, successor) {
    const view = store.activation?.view ?? store.sessionView, d = view?.descriptor;
    if (!d || receipt.previous_revision !== d.view_revision || receipt.previous_head !== d.head
        || !integer(receipt.view_revision, 1) || receipt.view_revision <= d.view_revision
        || receipt.entry?.id !== receipt.entry_id || receipt.entry?.parent !== receipt.previous_head) invalid('session append view receipt');
    const h = view.document, p = this.profile;
    // These are the exact changes to the compact six-field history document.
    const delta = jsonBytes(receipt.entry, p.snapshot_bytes) + (h.entries.length ? 1 : 0)
      + jsonBytes(receipt.entry_id, p.snapshot_bytes) + (h.branch_ids.length ? 1 : 0)
      + jsonBytes(receipt.head, p.snapshot_bytes) - jsonBytes(h.head, p.snapshot_bytes);
    if (delta < 0 || d.bytes + delta > p.snapshot_bytes || this.bytes + delta > p.generation_bytes
        || d.entry_count + 1 > p.view_entries || d.branch_count + 1 > p.view_entries || this.records + 2 > p.generation_entries) throw unavailable();
    this.bytes += delta; this.records += 2;
    const release = view.release; view.release = () => { this.bytes -= delta; this.records -= 2; release(); };
    h.entries.push(receipt.entry); h.branch_ids.push(receipt.entry_id); h.head = receipt.head;
    view.host.session_branch.push(receipt.entry); view.host.session_leaf_id = receipt.head;
    view.descriptor = { ...d, bytes: d.bytes + delta, entry_count: d.entry_count + 1, branch_count: d.branch_count + 1, head: receipt.head, view_revision: receipt.view_revision };
    // Parent and child linked by the native driver share this receipt chain.
    if (store.activation) store.activation.leaf.grant = successor;
  }
  async projection(result, store) {
    if (result.provider_context === undefined) return result;
    // Serialization validates the canonical 64MiB admission, not a frame cap.
    const size = jsonBytes(result.provider_context, this.profile.projection_bytes);
    if (size > this.profile.projection_bytes || this.projectionBytes + size > this.profile.projection_bytes || this.projections >= this.profile.projections) throw unavailable();
    if (jsonBytes({ jsonrpc: '2.0', id: store.id, result }, this.profile.projection_bytes + 65536) <= this.frame) return result;
    this.projectionBytes += size; this.projections++;
    try {
      const bytes = Buffer.from(JSON.stringify(result.provider_context)), sha256 = hash(bytes);
      const offer = await this.call(store, 'session/projection/begin', { bytes: size, sha256 });
      exact(offer, ['transfer_id', 'chunk_bytes'], 'projection offer');
      if (!hex(offer.transfer_id) || offer.chunk_bytes !== this.profile.chunk_bytes) invalid('projection offer');
      for (let offset = 0; offset < size;) {
        const end = Math.min(size, offset + offer.chunk_bytes);
        const ack = await this.call(store, 'session/projection/chunk', { transfer_id: offer.transfer_id, offset, data: bytes.subarray(offset, end).toString('base64') });
        exact(ack, ['next_offset'], 'projection acknowledgement'); if (ack.next_offset !== end) invalid('projection offset'); offset = end;
      }
      const committed = await this.call(store, 'session/projection/commit', { transfer_id: offer.transfer_id });
      exact(committed, ['transfer_id', 'bytes', 'sha256'], 'projection commit');
      if (committed.transfer_id !== offer.transfer_id || committed.bytes !== size || committed.sha256 !== sha256) invalid('projection commit identity');
      const { provider_context, ...rest } = result;
      return { ...rest, provider_context_transfer: committed };
    } finally { this.projectionBytes -= size; this.projections--; }
  }
}

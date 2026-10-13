import {closed, nominal, opaque} from './values.mjs';

const types = new WeakMap();
export function resourceType(name, dispose = () => {}) {
  if (!nominal(name) || typeof dispose !== 'function') throw new TypeError('Invalid resource type');
  const schema = {type: 'object', properties: {$resource: {type: 'string', minLength: 1, maxLength: 128}, type: {type: 'string', enum: [name]}}, required: ['$resource', 'type'], additionalProperties: false};
  const type = Object.freeze({name, schema}); types.set(schema, {type, dispose});
  return type;
}
export function resourceSlots(schema, path = '', inArray = false) {
  if (!schema) return [];
  const type = types.get(schema);
  if (type) {
    if (!path || inArray) throw new TypeError('Resources require fixed object fields, never arrays or root outputs');
    return [{path, type: type.type.name, declaration: type}];
  }
  return [...Object.entries(schema.properties ?? {}).flatMap(([key, child]) => resourceSlots(child, `${path}/${key.replaceAll('~', '~0').replaceAll('/', '~1')}`, inArray)),
    ...(schema.items ? resourceSlots(schema.items, path, true) : [])];
}
export function reference(value) {
  closed(value, ['$resource', 'type']);
  if (!opaque(value.$resource) || !nominal(value.type)) throw new TypeError('Invalid ResourceRef');
  return value;
}
const ownerKey = owner => {
  if (!owner) throw new TypeError('A host resource owner is required');
  return JSON.stringify([owner.session_id, owner.extension_instance_id, owner.process_generation]);
};
export class Resources {
  constructor(tools) {
    this.records = new Map(); this.identities = new WeakSet(); this.declarations = new Map(); this.pending = 0; this.retiredTokens = new Set(); this.allocated = 0;
    for (const tool of tools.values()) for (const slot of [...tool.inputs, ...tool.outputs]) {
      const previous = this.declarations.get(slot.type);
      if (previous && previous !== slot.declaration) throw new TypeError('Duplicate nominal resource declaration');
      this.declarations.set(slot.type, slot.declaration);
    }
  }
  admit(job, registration) {
    job.resources = new Set();
    for (const slot of registration.inputs) {
      let value = job.params.arguments;
      for (const key of slot.path.slice(1).split('/').map(k => k.replaceAll('~1', '/').replaceAll('~0', '~'))) value = value?.[key];
      if (value === undefined) continue;
      this.lookup(job, value); job.resources.add(value.$resource);
    }
  }
  lookup(job, value) {
    reference(value);
    const record = this.records.get(value.$resource);
    if (!record || record.reference.type !== value.type || record.owner !== ownerKey(job.params.context.resource_owner)) throw new TypeError('Resource unavailable');
    return record;
  }
  resolve(job, value) {
    const record = this.lookup(job, value);
    if (!job.resources?.has(value.$resource)) throw new TypeError('Resource not admitted to this call');
    return record.value;
  }
  async export(job, type, value, request) {
    const declaration = this.declarations.get(type?.name);
    if (!declaration || declaration.type !== type || value === null || !['object', 'function'].includes(typeof value)) throw new TypeError('Declare the resource schema before exporting an object');
    if (this.identities.has(value) || this.allocated + this.pending >= 256 || (job.exports ?? 0) >= 32) throw new TypeError('Resource alias or quota exceeded');
    const owner = ownerKey(job.params.context.resource_owner);
    job.exports = (job.exports ?? 0) + 1; this.pending++; this.identities.add(value);
    try {
      const ref = reference(await request('resource/register', {type: type.name}));
      if (ref.type !== type.name || this.records.has(ref.$resource) || this.retiredTokens.has(ref.$resource)) throw new TypeError('Invalid registered identity');
      const frozen = Object.freeze({...ref});
      this.records.set(ref.$resource, {reference: frozen, value, declaration, owner}); this.allocated++;
      job.resources.add(ref.$resource);
      return frozen;
    } catch (error) { this.identities.delete(value); throw error; }
    finally { this.pending--; }
  }
  retire(params) {
    closed(params, ['resources', 'reason']);
    if (params.reason !== 'retired' || !Array.isArray(params.resources) || !params.resources.length || params.resources.length > 256) throw new TypeError('Invalid disposal batch');
    const refs = params.resources.map(reference);
    if (new Set(refs.map(r => r.$resource)).size !== refs.length) throw new TypeError('Duplicate disposal');
    for (const r of refs) if (this.records.has(r.$resource) && this.records.get(r.$resource).reference.type !== r.type) throw new TypeError('Disposal type mismatch');
    if (this.retiredTokens.size + refs.filter(r => !this.retiredTokens.has(r.$resource)).length > 65536) throw new TypeError('Disposal identity bound exceeded');
    return refs.map(resource => {
      this.retiredTokens.add(resource.$resource);
      const record = this.records.get(resource.$resource); this.records.delete(resource.$resource);
      return {resource, record};
    });
  }
  async dispose(retired) {
    const results = [];
    for (const {resource, record} of retired) {
      let status = record ? 'completed' : 'failed';
      if (record) {
        try { await record.declaration.dispose(record.value); } catch { status = 'failed'; }
        this.identities.delete(record.value); this.allocated--;
      }
      results.push({resource, status});
    }
    return {results};
  }
  async shutdown() {
    const retired = [...this.records.values()].map(record => ({resource: record.reference, record}));
    this.records.clear(); await this.dispose(retired);
  }
}

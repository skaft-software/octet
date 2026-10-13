import {Extension, resourceType, blobSchema, validateBlob, HostRequestError} from '../../process/index.mjs';
const ext = new Extension({features: ['artifacts', 'composer']});
const Counter = resourceType('example.Counter', value => { console.error(`disposed:${value.n}`); if (value.n < 0) throw new Error('cleanup failed'); });
const empty = {type: 'object', properties: {}, additionalProperties: false};
const refRecord = {type: 'object', properties: {counter: Counter.schema}, required: ['counter'], additionalProperties: false};
let saved, previous;
ext.typedTool({name: 'create', description: 'Create counter', parameters: empty, outputSchema: refRecord}, async (_args, ctx) => {
  saved = await ctx.exportResource(Counter, {n: 0}); return {counter: saved};
}, () => 'Created counter');
ext.typedTool({name: 'increment', description: 'Increment counter', parameters: refRecord, outputSchema: {type: 'integer'}, receiver: '/counter'}, ({counter}, ctx) => {
  console.error('increment entered'); return ++ctx.resource(counter).n;
}, String);
ext.tool({name: 'release', description: 'Release saved counter', parameters: empty}, async (_args, ctx) => {
  const status = await ctx.releaseResource(saved); return JSON.stringify(status);
});
ext.tool({name: 'service', description: 'Exercise author services', parameters: {type: 'object', properties: {mode: {type: 'string'}}, required: ['mode'], additionalProperties: false}}, async ({mode}, ctx) => {
  if (mode === 'diagnostic') return {text: 'Domain failure', isError: true, diagnostics: [{severity: 'error', code: 'solver.failed', message: 'Failed\nnow'}]};
  if (mode === 'invalid-diagnostic') return {text: 'Invalid', diagnostics: [{severity: 'error', code: 'x', message: '\u001b[31m'}]};
  if (mode === 'media') {
    const id = await ctx.publishArtifact(new Uint8Array([1, 2, 3]), 'image/png');
    return {text: 'Image', media: [{type: 'image', artifact_id: id, mime_type: 'image/png', alt: 'Preview'}]};
  }
  if (mode === 'stale') return JSON.stringify(await previous.request('composer/get'));
  if (mode === 'forged') return JSON.stringify(await ctx.request('composer/get', {parent_request_id: 44}));
  if (mode === 'unawaited') { void ctx.request('composer/get'); return 'Finished'; }
  previous = ctx;
  try { return JSON.stringify(await ctx.request('composer/get')); }
  catch (error) { if (error instanceof HostRequestError) return {text: `Host error ${error.code}`, isError: true}; throw error; }
});
ext.tool({name: 'blob', description: 'Low-level bulk transfer grants', parameters: empty, outputSchema: blobSchema}, async (_args, ctx) => {
  const ticket = await ctx.request('bulk/write', {profile: 'local-file.v1', capacity: 0, media_type: 'application/octet-stream'});
  const blob = validateBlob(await ctx.request('bulk/commit', {ticket: ticket.ticket, bytes: 0, digest: {algorithm: 'sha256', value: 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'}}));
  return {text: 'Empty blob', structuredContent: blob};
});
ext.hook('before_prompt', ({prompt}) => ({context: [{label: 'note', content: prompt, placement: 'prompt_suffix'}]}));
ext.hook('cache_warming_decision', () => ({cache_warming_decision: 'stop'}));
ext.hook('model_turn_start', async (payload, ctx) => {
  for (let i = 0; i < Number(payload.append || 0); i++) await ctx.request('session/append_entry', {entry_type: 'custom', data: {observed: true}});
  return payload.bad ? {session_operation: {action: 'cancel'}} : {session_operation: {action: 'continue'}};
});
export default ext;

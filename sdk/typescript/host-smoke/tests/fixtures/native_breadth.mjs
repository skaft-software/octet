import {appendFileSync} from 'node:fs';
import {Extension, resourceType, HostRequestError} from '../../../process/index.mjs';

const record = (event, details = {}) => appendFileSync(process.env.OCTET_TS_TYPED_LOG,
  JSON.stringify({event, pid: process.pid, ...details}) + '\n');
const extension = new Extension({features: ['artifacts', 'policy_intents']});
extension.onShutdown(({reason}) => record('shutdown', {reason}));
const empty = {type: 'object', properties: {}, additionalProperties: false};
const Counter = resourceType('native.Counter', value => {
  record('disposed', {value: value.n});
  if (value.n < 0) throw new Error('Expected disposer failure');
});
const refs = {type: 'object', properties: {counter: Counter.schema}, required: ['counter'], additionalProperties: false};
let saved;
extension.typedTool({name: 'create', description: 'Create native state', parameters: {
  type: 'object', properties: {value: {type: 'integer'}, invalid: {type: 'boolean'}}, required: ['value'], additionalProperties: false,
}, outputSchema: refs}, async ({value, invalid}, context) => {
  saved = await context.exportResource(Counter, {n: value});
  record('exported', {resource: saved});
  return invalid ? {counter: saved, extra: true} : {counter: saved};
}, () => 'Created counter');
extension.typedTool({name: 'increment', description: 'Use admitted native state', parameters: refs,
  outputSchema: {type: 'integer'}, receiver: '/counter'}, ({counter}, context) => {
  record('increment');
  return ++context.resource(counter).n;
}, String);
extension.tool({name: 'release', description: 'Retire saved unpinned state', parameters: empty}, async (_, context) => {
  const status = await context.releaseResource(saved);
  record('released', {status});
  return JSON.stringify(status);
});
extension.tool({name: 'service', description: 'Real reverse request with typed diagnostic output', parameters: {
  type: 'object', properties: {invalid: {type: 'boolean'}}, additionalProperties: false,
}, outputSchema: {type: 'object', properties: {decision: {type: 'string', enum: ['deny']}}, required: ['decision'], additionalProperties: false}}, async ({invalid}, context) => {
  const result = await context.request('policy/evaluate', {intent: {
    kind: 'external_side_effect', operation: 'fixture.inspect', target: {label: 'Provider-free probe'},
    data_classes: [], adapter_hints: {read_only: true, destructive: false},
  }});
  record('policy', {result});
  return {text: 'Policy checked', structuredContent: {decision: result.decision},
    diagnostics: [{severity: 'warning', code: 'policy.denied', message: invalid ? '\u001b[31m' : 'No effects\nperformed'}]};
});
extension.tool({name: 'media', description: 'Publish verified native artifact', parameters: {
  type: 'object', properties: {invalid: {type: 'boolean'}}, additionalProperties: false,
}}, async ({invalid}, context) => {
  // The production host validates a real one-pixel PNG, not a mocked artifact ID.
  const bytes = invalid ? Buffer.from([1, 2, 3]) : Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jv1sAAAAASUVORK5CYII=', 'base64');
  try {
    const id = await context.publishArtifact(bytes, 'image/png');
    record('artifact', {id});
    return {text: 'Verified preview', media: [{type: 'image', artifact_id: id, mime_type: 'image/png', alt: 'One pixel'}]};
  } catch (error) {
    if (!(error instanceof HostRequestError)) throw error;
    record('artifact-refused', {code: error.code});
    return {text: `Host refused artifact: ${error.code}`, isError: true};
  }
});
extension.hook('before_prompt', ({prompt}) => {
  record('hook');
  return {context: [{label: 'native-note', content: prompt, placement: 'prompt_suffix'}]};
});
export default extension;

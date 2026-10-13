import { Extension, ToolError, resourceType, blobSchema, type BlobRef, type RequestContext, type InferSchema } from '../process/index.mjs';
const extension = new Extension({maxConcurrentRequests: 2});
const counter = resourceType<{n: number}>('example.Counter', value => { value.n = 0; });
extension.typedTool({name: 'counter', description: 'Native counter', parameters: {type: 'object'},
  outputSchema: {type: 'object', properties: {counter: counter.schema}, required: ['counter'], additionalProperties: false},
}, async (_args, ctx) => ({counter: await ctx.exportResource(counter, {n: 0})}), () => 'Counter');
extension.tool({name: 'add', description: 'Native increment', receiver: '/counter',
  parameters: {type: 'object', properties: {counter: counter.schema}, required: ['counter'], additionalProperties: false},
}, (args, ctx) => {
  const native: {n: number} = ctx.resource(args.counter);
  // @ts-expect-error Native resources retain their declared type.
  const wrong: {text: string} = ctx.resource(args.counter);
  void wrong;
  return {text: String(++native.n), diagnostics: [{severity: 'info', code: 'counter.add', message: 'Incremented'}]};
});
const blob: InferSchema<typeof blobSchema> = {} as BlobRef;
void blob;
extension.hook('cache_warming_decision', () => ({cache_warming_decision: 'stop'}));
extension.hook('model_turn_start', (_payload, context) => { context.throwIfCancelled(); return {session_operation: {action: 'continue'}}; });
// @ts-expect-error No speculative hook name.
extension.hook('imaginary_hook', () => {});
// @ts-expect-error Required feature names are a bounded known authoring set.
new Extension({features: ['arbitrary_wire']});
extension.tool({name: 'typed', description: 'Inference test', parameters: {
  type: 'object', properties: {
    text: {type: 'string'}, wait: {type: 'integer'},
    mode: {type: 'string', enum: ['fast', 'slow']},
    values: {type: 'array', items: {type: 'number'}},
    enabled: {type: 'boolean'},
  }, required: ['text', 'mode'], additionalProperties: false,
}}, async (args, context) => {
  const text: string = args.text;
  const mode: 'fast' | 'slow' = args.mode;
  const wait: number | undefined = args.wait;
  const values: number[] | undefined = args.values;
  const enabled: boolean | undefined = args.enabled;
  // @ts-expect-error Required inferred string is not a number.
  const wrong: number = args.text;
  // @ts-expect-error Optional property must be narrowed.
  const missing: number = args.wait;
  // @ts-expect-error No invented argument.
  args.secret;
  const signal: AbortSignal = context.signal;
  context.throwIfCancelled();
  await context.sleep(wait ?? 0);
  if (context.supportsProgress) await context.progress(mode, {current: 1, total: 1});
  if (enabled) throw new ToolError('Explicit failure');
  return {text: text + String(values?.length) + String(signal.aborted), isError: false};
});
extension.command({name: 'typed-command', description: 'Command'}, (args, context) => args.join(' ') + context.workspace);
extension.onShutdown(async ({reason}) => { const terminal: 'shutdown' | 'transport_lost' = reason; void terminal; });
// @ts-expect-error Tool needs a schema rather than silently accepting arbitrary arguments.
extension.tool({name: 'missing', description: 'Missing'}, () => 'x');
// @ts-expect-error Unsupported author result shape.
extension.tool({name: 'invalid', description: 'Invalid', parameters: {type: 'object'}}, () => ({content: 'raw wire'}));
const nested = {type: 'object', properties: {inner: {type: 'object', properties: {ok: {type: 'boolean'}}, required: ['ok']}}, required: ['inner']} as const;
const value: InferSchema<typeof nested> = {inner: {ok: true}};
void value;
const useContext = (ctx: RequestContext) => ctx.resource_owner?.process_generation;
void useContext;
extension.typedTool({name: 'square', description: 'Typed result',
  parameters: {type: 'object', properties: {x: {type: 'number'}}, required: ['x'], additionalProperties: false},
  outputSchema: {type: 'object', properties: {value: {type: 'number'}}, required: ['value'], additionalProperties: false},
}, ({x}) => ({value: x * x}), result => {
  const value: number = result.value;
  // @ts-expect-error The result projection receives its schema-inferred fields.
  result.missing;
  return String(value);
});
extension.typedTool({name: 'invalid-output', description: 'Output type check', parameters: {type: 'object'}, outputSchema: {type: 'integer'}},
  // @ts-expect-error A string is not an integer output.
  () => 'not a number', String);

const recordSchema = {type: 'object', properties: {
  count: {type: 'integer'}, note: {type: 'null'}, label: {type: 'string'},
  mode: {type: 'string', enum: ['ready', 'empty']},
  rows: {type: 'array', items: {type: 'object', properties: {ok: {type: 'boolean'}}, required: ['ok']}},
}, required: ['count', 'note', 'mode', 'rows'], additionalProperties: false} as const;
const record: InferSchema<typeof recordSchema> = {count: 1, note: null, mode: 'ready', rows: [{ok: true}]};
// @ts-expect-error Missing is not silently converted into explicit null.
const missingNull: InferSchema<typeof recordSchema> = {count: 1, mode: 'empty', rows: []};
// @ts-expect-error An optional string is not nullable.
const nullableLabel: InferSchema<typeof recordSchema> = {...record, label: null};
// @ts-expect-error Homogeneous array items retain the nested record schema.
const wrongRow: InferSchema<typeof recordSchema> = {...record, rows: [{ok: 1}]};
// @ts-expect-error Scalar enum inference preserves its literal alternatives.
const wrongMode: InferSchema<typeof recordSchema> = {...record, mode: 'other'};
void [missingNull, nullableLabel, wrongRow, wrongMode];

extension.typedTool({name: 'record', description: 'Nested structured output',
  parameters: {type: 'object'}, outputSchema: recordSchema,
}, async (_args, context) => {
  context.throwIfCancelled();
  return record;
}, result => {
  const count: number = result.count;
  const note: null = result.note;
  const label: string | undefined = result.label;
  const mode: 'ready' | 'empty' = result.mode;
  const rows: {ok: boolean}[] = result.rows;
  return `${count} ${note} ${label} ${mode} ${rows.length}`;
});
extension.typedTool({name: 'null', description: 'Explicit null', parameters: {type: 'object'}, outputSchema: {type: 'null'}},
  () => null, value => { const explicitNull: null = value; return String(explicitNull); });
extension.tool({name: 'lower-null', description: 'Low-level null', parameters: {type: 'object'}, outputSchema: {type: 'null'}},
  () => ({text: 'No value', structuredContent: null}));
extension.tool({name: 'lower-invalid', description: 'Low-level output inference', parameters: {type: 'object'}, outputSchema: {type: 'integer'}},
  // @ts-expect-error The lower-level API also checks schema-inferred output values.
  () => ({text: 'Invalid', structuredContent: 'wrong'}));
// @ts-expect-error typedTool requires an output schema even if the handler returns a value.
extension.typedTool({name: 'missing-output-schema', description: 'Missing', parameters: {type: 'object'}}, () => null, String);
// @ts-expect-error A model-facing text projection must be explicit, not auto-stringified JSON.
extension.typedTool({name: 'missing-projection', description: 'Missing', parameters: {type: 'object'}, outputSchema: {type: 'null'}}, () => null);
extension.typedTool({name: 'wrong-projection', description: 'Projection type check', parameters: {type: 'object'}, outputSchema: {type: 'integer'}},
  () => 1,
  // @ts-expect-error A projection must return text rather than another structured value.
  value => ({value}));

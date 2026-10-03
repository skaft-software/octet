import { Extension, ToolError, type RequestContext, type InferSchema } from '../process/index.mjs';
const extension = new Extension({maxConcurrentRequests: 2});
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

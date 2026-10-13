import { Extension, ToolError } from '../../process/index.mjs';

const extension = new Extension({maxConcurrentRequests: 2});
let calls = 0;
let previousContext;
extension.tool({
  name: 'test_tool', description: 'Process contract fixture',
  parameters: {type: 'object', properties: {
    text: {type: 'string', maxLength: 1024},
    mode: {type: 'string', enum: ['echo', 'error', 'throw', 'raw', 'large', 'invalid', 'never', 'progress', 'stale', 'owner', 'flood', 'diagnostics']},
    ms: {type: 'integer', minimum: 0, maximum: 30_000},
  }, additionalProperties: false},
}, async (args, context) => {
  calls++;
  context.throwIfCancelled();
  switch (args.mode) {
    case 'throw': throw new Error('PRIVATE-EXCEPTION');
    case 'error': throw new ToolError('Expected tool failure');
    case 'raw': process.stdout.write('NOT-PROTOCOL'); break;
    case 'large': return 'x'.repeat(1_048_576);
    case 'invalid': return {content: 'raw-wire-result'};
    case 'never': return new Promise(() => {});
    case 'stale': await previousContext.progress('must not emit'); break;
    case 'owner': return JSON.stringify(context.resource_owner);
    case 'flood': await Promise.all(Array.from({length: 200}, () => context.progress('flood'))); break;
    case 'diagnostics':
      console.dir({message: 'console.dir stays off stdout'});
      console.table([{message: 'console.table stays off stdout'}]);
      for (let n = 0; n < 100; n++) console.log('x'.repeat(8192));
      break;
    case 'progress':
      previousContext = context;
      await context.progress('started', {current: 0, total: 1, unit: 'steps'});
      await context.sleep(args.ms ?? 1);
      await context.progress('finished', {current: 1, total: 1});
      break;
    default:
      if (context.supportsProgress) await context.progress('started');
      await context.sleep(args.ms ?? 0);
  }
  console.log('diagnostic from author');
  return `${args.text ?? 'hello'}:${calls}`;
});
extension.command({name: 'test-command', description: 'Fixture command'}, (args, context) => `${args.join(',')}@${context.workspace}`);
extension.onShutdown(async ({reason}) => {
  console.error(`shutdown:${reason}`);
  if (process.env.STUCK_SHUTDOWN) await new Promise(() => {});
});
export default extension;

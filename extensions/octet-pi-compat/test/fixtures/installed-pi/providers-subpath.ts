import { builtinModels, getBuiltinModels } from '@earendil-works/pi-ai/providers/all';
import { builtinProviders } from '@mariozechner/pi-ai/providers/all';
export default function (pi: any) {
  const model = getBuiltinModels('anthropic')[0];
  let guarded = false;
  try { builtinModels().getProvider('anthropic')!.stream(model, { messages: [] }); }
  catch (error) { guarded = /admitted provider request/.test(String(error)); }
  pi.registerCommand('providers-subpath', {
    description: `guarded=${guarded} providers=${builtinProviders().length > 0}`,
    handler: async () => {},
  });
}

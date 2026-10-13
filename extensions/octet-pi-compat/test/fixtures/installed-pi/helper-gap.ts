// The gap is in a relative helper module, not the entrypoint.
import { helperValue } from './helper-gap-lib';
export default function (pi: any) { pi.registerCommand('helper', { description: helperValue(), handler: async () => {} }); }

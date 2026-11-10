// Pi's examples/extensions/commands.ts shape: one command awaits
// ctx.ui.select(...) and then ctx.ui.confirm(...).
export default function (pi) {
  pi.registerCommand('pick-then-confirm', { description: 'select then confirm', async handler(_, ctx) {
    const picked = await ctx.ui.select('Available Commands', ['one', 'two']);
    const confirmed = await ctx.ui.confirm('View source path?', 'details');
    ctx.ui.notify(`result:${picked}:${confirmed}`);
  } });
  pi.registerCommand('pick-then-input', { description: 'select then input', async handler(_, ctx) {
    const picked = await ctx.ui.select('Pick', ['one', 'two']);
    const typed = await ctx.ui.input('Name');
    ctx.ui.notify(`typed:${picked}:${typed}`);
  } });
}

// A factory whose own async setup never settles, like one that awaits a network
// call before registering anything.
export default async pi => {
  pi.registerCommand('stalled-late', { description: 'registered before the stall', handler() {} });
  await new Promise(() => {});
  pi.registerCommand('never-seen', { description: 'unreachable', handler() {} });
};

// A module whose top-level await never settles, like one that fetches before it
// exports its factory.
await new Promise(() => {});
export default pi => pi.registerCommand('never-seen', { description: 'unreachable', handler() {} });

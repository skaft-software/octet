export default function (pi) {
  pi.registerCommand('desktop-notify', { description: 'Exercise terminal notification intent', handler: async (args) => {
    const body = args === 'unsafe' ? 'hello\x1b]2;injected\x07' : args === 'oversize' ? 'x'.repeat(4097) : 'Hello from the mock provider.';
    process.stdout.write('\x1b]777;notify;π;' + body + '\x07');
  } });
}

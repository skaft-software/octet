// Native-host primitive fixture, NOT an unchanged CLM qualification gate.
// Run through runner.mjs with a real process leaf binding and sole Rust Session
// consumer. The fixture must never be answered with a synthetic persistence ACK
// when cited as native evidence.
export default function nativeSyncLeaf(pi) {
  pi.on('before_agent_start', () => {
    const returned = pi.appendEntry('native-sync-proof', { version: 1, text: 'native\nprivate\r\t🙂' });
    if (returned !== undefined) throw new Error('appendEntry must be synchronous void');
    console.error('[native-sync-proof] append returned after host commit reply');
  });
}

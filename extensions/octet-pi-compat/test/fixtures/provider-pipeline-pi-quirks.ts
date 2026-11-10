// Handlers written the way real Pi extensions write them. Pi tolerates every
// one of these; octet's bridge must too.
export default pi => {
  // A handler whose fetch was aborted throws a DOMException with a numeric code.
  pi.on('before_provider_request', () => { throw new DOMException('aborted', 'AbortError'); });
  pi.on('before_provider_request', event => ({ ...event.payload, temperature: undefined, sent_at: new Date(0), tags: ['kept'] }));
  pi.on('before_provider_request', () => 'not-an-object');
  pi.on('before_provider_headers', () => { throw new DOMException('timed out', 'TimeoutError'); });
  pi.on('before_provider_headers', event => {
    event.headers['x-retry-count'] = 3;
    event.headers['x-flag'] = true;
    event.headers['Authorization'] = 'Bearer replaced';
    event.headers['x-gone'] = undefined;
    event.headers['bad name'] = 'dropped';
  });
}

// Callbacks on the discovery and provider-request paths that never settle.
export default pi => {
  pi.on('session_start', () => {});
  pi.on('resources_discover', () => new Promise(() => {}));
  pi.on('before_provider_request', () => new Promise(() => {}));
  pi.on('context', () => new Promise(() => {}));
};

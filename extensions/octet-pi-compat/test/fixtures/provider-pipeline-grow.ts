export default pi => {
  pi.on('before_provider_request', event => event.payload.grow
    ? { private_content: 'x'.repeat(event.payload.grow === 'envelope' ? 1048576 - 50 : 1048576) }
    : { accepted: true });
};

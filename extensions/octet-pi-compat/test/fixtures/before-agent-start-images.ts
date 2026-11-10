// Reports the run images Pi puts on BeforeAgentStartUserEvent. A text-only run
// must omit the member entirely, so the fixture records its presence rather than
// relying on the value alone.
export default pi => {
  pi.on('before_agent_start', event => ({ systemPrompt: JSON.stringify({
    prompt: event.prompt,
    hasImages: Object.hasOwn(event, 'images'),
    images: event.images,
  }) }));
}

// Pure Pi 1.0.2 public pi-ai helpers, adapted from MIT-licensed models.ts and
// utils/{transcript,text,event-stream}.ts at 200387122ca450d6387f033949423114a270b96c.
// See ../LICENSE.pi. Caller-supplied data only: no provider, model registry,
// credentials, session, terminal, or child SDK runtime is loaded.

/**
 * Mutates and returns the caller's existing Pi Usage.cost object ($, not native
 * microdollars). The highest strictly exceeded input threshold prices the whole
 * request; one-hour cache writes cost twice that tier's input rate.
 * @param {import('@earendil-works/pi-ai').AnyModel} model
 * @param {import('@earendil-works/pi-ai').Usage} usage
 * @returns {import('@earendil-works/pi-ai').Usage['cost']}
 */
export function calculateCost(model, usage) {
  const inputTokens = usage.input + usage.cacheRead + usage.cacheWrite;
  let rates = model.cost;
  let matchedThreshold = -1;
  for (const tier of model.cost.tiers ?? []) {
    if (inputTokens > tier.inputTokensAbove && tier.inputTokensAbove > matchedThreshold) {
      rates = tier;
      matchedThreshold = tier.inputTokensAbove;
    }
  }
  const longWrite = usage.cacheWrite1h ?? 0;
  const shortWrite = usage.cacheWrite - longWrite;
  usage.cost.input = (rates.input / 1000000) * usage.input;
  usage.cost.output = (rates.output / 1000000) * usage.output;
  usage.cost.cacheRead = (rates.cacheRead / 1000000) * usage.cacheRead;
  usage.cost.cacheWrite = (rates.cacheWrite * shortWrite + rates.input * 2 * longWrite) / 1000000;
  usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cacheRead + usage.cost.cacheWrite;
  return usage.cost;
}

const contentText = content => typeof content === 'string' ? content
  : content.filter(block => block.type === 'text').map(block => block.text).join('\n');

/** Replay removals before additions, retaining tool identity and Map order. */
export function getCurrentTools(messages) {
  const tools = new Map();
  for (const message of messages) {
    if (message.role !== 'system') continue;
    for (const tool of message.toolsRemoved ?? []) tools.delete(tool.name);
    for (const tool of message.toolsAdded ?? []) tools.set(tool.name, tool);
  }
  return [...tools.values()];
}

function getCurrentSystemMessage(messages) {
  const content = [], sections = new Map();
  let timestamp;
  for (const message of messages) {
    if (message.role !== 'system') continue;
    timestamp ??= message.timestamp;
    const text = contentText(message.content);
    if (text.length > 0) content.push(text);
    for (const [name, value] of Object.entries(message.sections ?? {})) {
      if (value === null) sections.delete(name);
      else sections.set(name, value);
    }
  }
  const tools = getCurrentTools(messages);
  if (timestamp === undefined && tools.length === 0) return undefined;
  return {
    role: 'system', content: content.join('\n\n'),
    ...(sections.size > 0 ? { sections: Object.fromEntries(sections) } : {}),
    ...(tools.length > 0 ? { toolsAdded: tools } : {}), timestamp: timestamp ?? 0,
  };
}

/** Append content, patch named sections (null deletes), then render sections. */
export function getCurrentSystemPrompt(messages) {
  const message = getCurrentSystemMessage(messages);
  if (!message) return '';
  const parts = [contentText(message.content)];
  for (const text of Object.values(message.sections ?? {})) {
    if (text !== null) parts.push(text);
  }
  return parts.filter(part => part.length > 0).join('\n\n');
}

/** Return a new Pi transcript envelope; non-system messages keep their identity. */
export function collapseSystemMessages(context) {
  const head = getCurrentSystemMessage(context.messages);
  const messages = context.messages.filter(message => message.role !== 'system');
  return { messages: head ? [head, ...messages] : messages };
}

// Pi uses a two-stack FIFO so repeated push/consume stays amortized O(1).
class FifoQueue {
  incoming = [];
  outgoing = [];
  get length() { return this.incoming.length + this.outgoing.length; }
  enqueue(value) { this.incoming.push(value); }
  dequeue() {
    if (this.outgoing.length === 0) {
      while (this.incoming.length > 0) this.outgoing.push(this.incoming.pop());
    }
    return this.outgoing.pop();
  }
}

class EventStream {
  queue = new FifoQueue();
  waiting = new FifoQueue();
  done = false;
  constructor(isComplete, extractResult) {
    this.isComplete = isComplete;
    this.extractResult = extractResult;
    this.finalResultPromise = new Promise(resolve => { this.resolveFinalResult = resolve; });
  }
  push(event) {
    if (this.done) return;
    if (this.isComplete(event)) {
      this.done = true;
      this.resolveFinalResult(this.extractResult(event));
    }
    const waiter = this.waiting.dequeue();
    if (waiter) waiter({ value: event, done: false });
    else this.queue.enqueue(event);
  }
  end(result) {
    this.done = true;
    if (result !== undefined) this.resolveFinalResult(result);
    while (this.waiting.length > 0) {
      this.waiting.dequeue()({ value: undefined, done: true });
    }
  }
  async *[Symbol.asyncIterator]() {
    while (true) {
      if (this.queue.length > 0) yield this.queue.dequeue();
      else if (this.done) return;
      else {
        const result = await new Promise(resolve => this.waiting.enqueue(resolve));
        if (result.done) return;
        yield result.value;
      }
    }
  }
  result() { return this.finalResultPromise; }
}

class AssistantMessageEventStream extends EventStream {
  constructor() {
    super(event => event.type === 'done' || event.type === 'error', event => {
      if (event.type === 'done') return event.message;
      if (event.type === 'error') return event.error;
      throw new Error('Unexpected event type for final result');
    });
  }
}

/**
 * A caller-driven async event queue, not an inference stream. Terminal error
 * events resolve result() with their Pi assistant message, rather than reject.
 * @returns {import('@earendil-works/pi-ai').AssistantMessageEventStream}
 */
export function createAssistantMessageEventStream() {
  return new AssistantMessageEventStream();
}

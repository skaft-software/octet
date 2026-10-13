// Bounded Pi observations built only from actual native child-run events.
// This view is not a transcript writer and never feeds a provider or accounting.
import { bounded, invalid, plainJSON, unsupported } from './errors.mjs';
const MAX_HISTORY = 4 * 1024 * 1024;
const API = { open_ai_responses: 'openai-responses', open_ai_chat: 'openai-completions', anthropic_messages: 'anthropic-messages', google_generative_ai: 'google-generative-ai' };
const STOP = { EndTurn: 'stop', StopSequence: 'stop', ToolUse: 'toolUse', MaxTokens: 'length' };
const counter = value => { if (!Number.isSafeInteger(value) || value < 0) invalid('child usage counter'); return value; };
export function childUsage(usage, cost) {
  if (!usage || !cost) unsupported('child usage', 'native unpriced/uncertain cost cannot be represented as a fictional Pi zero');
  const dollars = value => counter(value) / 1000000;
  return {
    input: counter(usage.input_tokens), output: counter(usage.output_tokens), cacheRead: counter(usage.cache_read_tokens), cacheWrite: counter(usage.cache_write_tokens), totalTokens: counter(usage.total_tokens),
    cost: { input: dollars(cost.input), output: dollars(cost.output) + dollars(cost.reasoning ?? 0), cacheRead: dollars(cost.cache_read), cacheWrite: dollars(cost.cache_write), total: dollars(cost.total) + counter(cost.total_picodollars_remainder ?? 0) / 1e12 },
  };
}
function contentParts(parts) {
  if (!Array.isArray(parts) || parts.length > 256) invalid('child assistant parts');
  return parts.flatMap(part => {
    if (typeof part.Text === 'string') return [{ type: 'text', text: bounded(part.Text, 'child text', 262144, { controls: true }) }];
    if (part.Reasoning) {
      // Only the visible projection is exposed, never encrypted continuation
      // state. Native persistence, not these objects, owns provider replay.
      return typeof part.Reasoning.text === 'string' ? [{ type: 'thinking', thinking: bounded(part.Reasoning.text, 'child thinking', 262144, { controls: true }) }] : [];
    }
    if (part.ToolCall) {
      const call = part.ToolCall;
      if (call.argument_error) unsupported('child malformed tool call', 'cannot invent parsed arguments');
      let args; try { args = JSON.parse(call.arguments_json); } catch { invalid('child tool call arguments'); }
      return [{ type: 'toolCall', id: bounded(call.id, 'child tool id', 256), name: bounded(call.name, 'child tool name', 128), arguments: plainJSON(args, 'child tool arguments', 262144) }];
    }
    if (part.ProviderMetadata) return []; // private native replay metadata, not visible message content
    unsupported('child assistant media', 'media projection is not implemented');
  });
}

export class ChildEventProjection {
  constructor(emit) {
    this.emit = emit; this.messages = []; this.bytes = 0; this.pendingTools = new Map();
    this.partial = undefined; this.turn = undefined; this.runStart = 0; this.runFinished = false;
    this.model = undefined; this.provider = undefined;
  }
  _emit(event) { this.emit(structuredClone(event)); }
  _append(message) {
    const bytes = Buffer.byteLength(JSON.stringify(message));
    if (this.bytes + bytes > MAX_HISTORY || this.messages.length >= 8192) unsupported('child message history', 'bounded mirror exceeded; no truncated transcript is returned');
    this.bytes += bytes; this.messages.push(message);
  }
  _endTurn() {
    if (!this.turn || this.turn.remaining.size) return;
    this._emit({ type: 'turn_end', message: this.turn.message, toolResults: this.turn.results });
    this.turn = undefined;
  }
  accept(raw) {
    const event = plainJSON(raw, 'native child event', 262144);
    if (typeof event.kind !== 'string' || !Number.isSafeInteger(event.timestamp) || event.timestamp < 0) invalid('native child event identity');
    switch (event.kind) {
      case 'run_started': {
        if (this.turn) invalid('child run started before prior turn settled');
        this.runStart = this.messages.length; this.runFinished = false;
        this._append({ role: 'user', content: [{ type: 'text', text: bounded(event.message, 'accepted child prompt', 131072, { controls: true }) }], timestamp: event.timestamp });
        this._emit({ type: 'agent_start' }); break;
      }
      case 'user_message': {
        const message = { role: 'user', content: [{ type: 'text', text: bounded(event.message, 'delivered child input', 262144, { controls: true }) }], timestamp: event.timestamp };
        this._append(message); this._emit({ type: 'message_start', message }); this._emit({ type: 'message_end', message }); break;
      }
      case 'turn_started':
        if (this.turn || this.partial) invalid('child turn started before prior message/turn settled');
        this.partial = { role: 'assistant', content: [], timestamp: event.timestamp, ...(this.model ? { model: this.model } : {}), ...(this.provider ? { provider: this.provider } : {}) };
        this._emit({ type: 'turn_start' });
        this._emit({ type: 'message_start', message: this.partial }); break;
      case 'output_delta': {
        if (!this.partial || !['text', 'reasoning'].includes(event.channel)) invalid('child delta without a matching message start');
        bounded(event.text, 'child delta', 262144, { controls: true });
        const type = event.channel === 'text' ? 'text' : 'thinking', key = type === 'text' ? 'text' : 'thinking';
        let part = this.partial.content.at(-1);
        if (part?.type !== type) { part = { type, [key]: '' }; this.partial.content.push(part); }
        part[key] += event.text;
        if (Buffer.byteLength(JSON.stringify(this.partial)) > 262144) unsupported('child partial message', 'bounded stream projection exceeded');
        this._emit({ type: 'message_update', message: this.partial, assistantMessageEvent: { type: `${type}_delta`, contentIndex: this.partial.content.length - 1, delta: event.text, partial: this.partial } }); break;
      }
      case 'turn_finished': {
        if (!this.partial) invalid('child message end without start');
        const stopReason = STOP[event.stop_reason];
        if (!stopReason) unsupported(`child stop reason ${event.stop_reason}`);
        const message = { role: 'assistant', content: contentParts(event.message?.content), model: event.message?.model, timestamp: event.timestamp, stopReason, usage: childUsage(event.usage, event.cost) };
        if (typeof message.model !== 'string' || !message.model) invalid('native child message model');
        if (this.provider) message.provider = this.provider;
        if (API[event.message.protocol]) message.api = API[event.message.protocol];
        this.partial = undefined;
        this._append(message); this._emit({ type: 'message_end', message });
        this.turn = { message, remaining: new Set(message.content.filter(p => p.type === 'toolCall').map(p => p.id)), results: [] };
        this._endTurn(); break;
      }
      case 'tool_started':
        if (!this.turn?.remaining.has(event.id) || this.pendingTools.has(event.id)) invalid('native child tool start without a committed matching tool call');
        this.pendingTools.set(event.id, event.name);
        this._emit({ type: 'tool_execution_start', toolCallId: event.id, toolName: event.name, args: event.arguments }); break;
      case 'tool_finished': {
        const toolName = this.pendingTools.get(event.id);
        if (!toolName || !this.turn?.remaining.has(event.id)) invalid('unmatched native child tool result');
        const content = event.error ? [{ type: 'text', text: event.error }] : event.content;
        if (!Array.isArray(content) || content.some(p => p.type !== 'text' || typeof p.text !== 'string')) unsupported('child tool result media');
        if (typeof event.is_error !== 'boolean') invalid('child tool result error flag');
        const result = { content, ...(event.metadata?.pi_details === undefined ? {} : { details: event.metadata.pi_details }) };
        this._emit({ type: 'tool_execution_end', toolCallId: event.id, toolName, result, isError: event.is_error });
        const message = { role: 'toolResult', toolCallId: event.id, toolName, content, isError: event.is_error, timestamp: event.timestamp, ...('details' in result ? { details: result.details } : {}) };
        this._append(message); this._emit({ type: 'message_end', message });
        this.turn.results.push(message); this.turn.remaining.delete(event.id); this.pendingTools.delete(event.id); this._endTurn(); break;
      }
      case 'run_finished':
        this.runFinished = true;
        if (event.reason === 'completed' && (this.turn || this.partial)) invalid('native child completed with unsettled observations');
        // Do not report terminal success until the manager's status/accounting
        // also settles. A native RunFinished precedes that boundary.
        break;
      case 'output_discarded': unsupported('child retry/discard projection', 'provisional output was discarded by native host; no fabricated Pi continuation'); break;
      case 'observation_error': unsupported('child observations', event.error); break;
      default: unsupported(`native child event ${event.kind}`);
    }
  }
  settled() {
    if (!this.runFinished) invalid('child status settled without a native run terminal event');
    this._emit({ type: 'agent_end', messages: this.messages.slice(this.runStart) });
    this.runFinished = false; this.partial = undefined; this.turn = undefined; this.pendingTools.clear();
  }
}

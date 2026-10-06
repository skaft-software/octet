// Reviewed Pi factory for the real Rust App renderer acceptance test. Only
// public Pi APIs are used; TRACE is replaced by pi_contract_support::pi_ui_app.
import { appendFileSync } from 'node:fs';

export default pi => {
  const record = value => appendFileSync(TRACE, JSON.stringify(value) + '\n');
  let version = 0;
  let invalidateTool;
  let toolsEnabled = true;

  pi.registerMessageRenderer('renderer-message', (message, options, theme) => ({
    render(width) {
      record({kind: 'message', message, options, width});
      return [theme.fg('success', `VIEW_MESSAGE w=${width}`)];
    },
  }));
  pi.registerEntryRenderer('renderer-entry', (entry, options, theme) => ({
    render(width) {
      record({kind: 'entry', entry, options, width, timestampMs: Date.parse(entry.timestamp)});
      return [theme.fg('warning', `VIEW_ENTRY w=${width}`)];
    },
  }));
  pi.registerMarkdownTransformer((text, context) => {
    if (!text.includes('SOURCE_ASSISTANT')) return text;
    record({kind: 'markdown', text, context, width: context.availableWidth});
    return `**VIEW_MARKDOWN w=${context.availableWidth}**`;
  });

  // This is a renderer for the real host read tool, not a replacement executor.
  const toolRenderer = {
    renderShell: 'default',
    renderCall(args, theme, context) {
      invalidateTool = context.invalidate;
      context.state.calls = (context.state.calls ?? 0) + 1;
      const calls = context.state.calls;
      return {
        render(width) {
          record({kind: 'call', args, toolCallId: context.toolCallId, calls,
            lastComponent: Boolean(context.lastComponent), width, version});
          return [theme.fg('accent', `VIEW_CALL v=${version} w=${width}`)];
        },
      };
    },
    renderResult(result, options, theme, context) {
      return {
        render(width) {
          record({kind: 'result', result, options, toolCallId: context.toolCallId, width, version});
          return [theme.fg('success', `VIEW_RESULT v=${version} w=${width}`)];
        },
      };
    },
  };
  pi.registerToolRenderer((name, next) => name === 'read' && toolsEnabled ? toolRenderer : next());

  pi.registerCommand('seed-renderers', {handler: async () => {
    pi.appendEntry('renderer-entry', {private: 'PRIVATE_ENTRY_SECRET', count: 7});
    await pi.sendMessage({customType: 'renderer-message', content: 'SOURCE_MESSAGE',
      display: true, details: {private: 'MESSAGE_DETAILS', count: 9}}, {triggerTurn: false});
    await pi.sendMessage({customType: 'renderer-message', content: 'HIDDEN_MESSAGE',
      display: false, details: {private: 'HIDDEN_DETAILS'}}, {triggerTurn: false});
  }});
  pi.registerCommand('invalidate-renderer', {handler: () => {
    if (!invalidateTool) throw new Error('real tool renderer has not run');
    version++;
    invalidateTool();
  }});
  pi.registerCommand('fallback-renderers', {handler: () => {
    toolsEnabled = false;
    pi.registerMessageRenderer('renderer-message', () => undefined);
    pi.registerEntryRenderer('renderer-entry', () => undefined);
    pi.registerMarkdownTransformer(text => text);
  }});
}

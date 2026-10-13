import { fileURLToPath } from 'node:url';

// Pi's interactive diagnostics put load errors and warnings under one
// [Extension issues] heading, with a path on its own line. Callback notices
// additionally have a process-lifetime (extension, event) ledger: reporting a
// problem does not unsubscribe the handler or change any fail-closed boundary.

// Diagnostics are plain text, never terminal instructions. Do not interpolate
// exception messages here: even an unsupported property name can be prompt data.
function display(value, maxBytes = 4096) {
  const plain = String(value).toWellFormed().replace(/[\x00-\x1f\x7f-\x9f\u2028\u2029]/gu,
    char => `\\u${char.charCodeAt(0).toString(16).padStart(4, '0')}`);
  if (Buffer.byteLength(plain) <= maxBytes) return plain;
  let result = '', bytes = 0;
  for (const char of plain) {
    bytes += Buffer.byteLength(char);
    if (bytes > maxBytes - 3) break;
    result += char;
  }
  return result + '…';
}

// A factory that stops with its own machine-readable code ("some_code: ...")
// is named by that code alone. A bare snake_case identifier carries no prompt
// text; everything after it stays redacted.
function reportedCode(text) {
  const code = /^([a-z][a-z0-9]*(?:_[a-z0-9]+)+)(?=[:\s]|$)/.exec(String(text))?.[1];
  return code && code.length <= 96 ? code : undefined;
}

export function loadIssue(failure) {
  const { entry, code, name, error = '' } = failure;
  let reason, next;
  if (error.startsWith('invalid_request reviewed factory changed:')) {
    reason = 'The reviewed extension file changed and was skipped.';
    next = 'Review the changed file, then rerun configure.mjs --reviewed with your original options.';
  } else if (code === 'ENOENT' || code === 'MODULE_NOT_FOUND' || code === 'ERR_MODULE_NOT_FOUND') {
    reason = 'The extension file or one of its dependencies was not found; the extension was skipped.';
    next = 'Restore the file or install its reviewed dependencies, then reconfigure and restart octet.';
  } else if (name === 'FactoryStalled') {
    reason = `The extension's ${error}; octet stopped waiting and skipped it (it may still be running).`;
    next = 'If it waits on the network or another service, check that it is reachable; otherwise update or disable this extension.';
  } else if (code === 'EACCES' || code === 'EPERM') {
    reason = 'The extension file or a dependency could not be read; the extension was skipped.';
    next = 'Check file permissions, then reconfigure and restart octet.';
  } else if (code === -32030) {
    reason = 'The extension needs Pi helpers that octet does not emulate and was skipped.';
    next = 'Review the installed-Pi fallback, then re-import with configure.mjs --reviewed --from-pi and your original --output, or disable this extension.';
  } else if (code === -32031) {
    reason = 'The configured managed Pi 1.0.2 runtime is unavailable; the extension was skipped.';
    next = 'Restore the managed Pi 1.0.2 installation, then reconfigure, or disable this extension.';
  } else if (error.startsWith('invalid_request duplicate registration ')) {
    reason = 'A registration conflicts with another extension; this extension was skipped.';
    next = 'Disable one of the conflicting extensions, then reconfigure and restart octet.';
  } else if (code === -32601) {
    reason = 'The extension uses an API or registration that octet does not support and was skipped.';
    next = 'Update octet and the extension, or disable this extension, then reconfigure.';
  } else if (name === 'SyntaxError' || error.startsWith('invalid_request default export must be a factory:')) {
    reason = 'The extension source is invalid or does not export a factory; it was skipped.';
    next = 'Fix the extension source or update it, then review and reconfigure.';
  } else if (reportedCode(error)) {
    reason = `The extension stopped while loading with "${reportedCode(error)}" and was skipped.`;
    next = 'Check its dependencies and settings; update or disable this extension, then reconfigure.';
  } else {
    reason = 'The extension failed while loading and was skipped (private error details redacted).';
    next = 'Check its dependencies and settings; update or disable this extension, then reconfigure.';
  }
  return { path: entry, reason, next };
}

// A factory left out of the mirror when the setup was captured: said once at
// start, with the one reason that is safe to show, never its raw error text.
export function excludedIssue({ entry, reason = '' }) {
  const stalled = /setup did not finish within [0-9.]+ s/.exec(reason)?.[0];
  const code = reportedCode(reason);
  return { path: entry,
    reason: stalled
      ? `The extension's ${stalled}; it was left out of the mirror.`
      : code ? `The extension stopped while loading with "${code}"; it was left out of the mirror.`
        : 'The extension could not be loaded by the adapter and was left out of the mirror (private error details redacted).',
    next: 'Update or disable this extension. octet tries it again the next time it starts.' };
}

function formatIssue(issue) {
  return `  ${display(issue.path)}\n    ${issue.reason}\n    Next: ${issue.next}`;
}

export class ExtensionIssues {
  constructor(runtime) {
    this.runtime = runtime; this.started = false;
    this.callbacks = new Set(); this.renderers = new Set();
    // Last extension-originated notice write, kept as an ordering barrier.
    this.pending = undefined;
  }
  publish(issues) {
    if (!issues.length) return;
    // There are at most 64 factories, and each diagnostic/path is bounded.
    // Keep every issue instead of slicing a fleet's later failures off the block.
    const message = issues.map(formatIssue).join('\n');
    console.error(`[Extension issues]\n${message}`);
    const runtime = this.runtime;
    if (!runtime.stopping && !runtime.transport.closed) this.pending = runtime.transport.notify('notification', {
      level: 'warning', title: '[Extension issues]', message,
    }).catch(error => runtime.transport.fail(error));
  }
  // Resolve once every notice published so far has been written to the host.
  // A later request reply must not overtake a notice the host has not seen.
  async flush() {
    while (this.pending) {
      const pending = this.pending;
      this.pending = undefined;
      await pending;
    }
  }
  inertRenderers() {
    const runtime = this.runtime;
    if (!runtime.features.has('remote_ui') || runtime.features.has('transcript_render_v1')) return [];
    const issues = [];
    for (const renderer of runtime.transcript.metadata()) {
      const path = runtime.config.extensions[renderer.factory];
      if (this.renderers.has(path)) continue;
      this.renderers.add(path);
      const names = [...renderer.messages, ...renderer.entries,
        ...(renderer.markdown ? ['markdown transformer'] : []), ...(renderer.tools ? [`${renderer.tools} tool renderer(s)`] : [])];
      issues.push({ path,
        reason: `Custom renderers are not shown; octet uses its default transcript view (${display(names.join(', '), 512)}).`,
        next: 'Keep the default view, or disable this extension if its custom rendering is required.' });
    }
    return issues;
  }
  startup() {
    if (this.started) return;
    this.started = true;
    this.publish([...(this.runtime.excludedFactories || []).map(excludedIssue), ...(this.runtime.loadFailures || []).map(loadIssue), ...this.inertRenderers()]);
  }
  // Called by the runtime after successful initialization or a late registration.
  reportInertRenderers() { if (this.started) this.publish(this.inertRenderers()); }
  firstEventIssue(event, path) {
    const key = JSON.stringify([path, event]);
    if (this.callbacks.has(key)) return false;
    this.callbacks.add(key); return true;
  }
  observation(event) {
    // Native model-end projection also feeds the message/agent-end mirror, even
    // without any direct Pi turn_end handler. Attribute every affected factory.
    const events = event === 'turn_end' ? [event, 'agent_end', 'message_start', 'message_update', 'message_end'] : [event];
    const paths = new Set(events.flatMap(name => (this.runtime.events.get(name) || []).map(entry => this.runtime.config.extensions[entry.factory])));
    // A pre-subscribed hook may have no Pi consumers at all. No native payload
    // or exception text enters either this fallback or an attributed notice.
    if (!paths.size) paths.add(fileURLToPath(new URL('./model-turns.mjs', import.meta.url)));
    this.publish([...paths].filter(path => this.firstEventIssue(event, path)).map(path => ({ path,
      reason: `${display(event, 128)}: octet could not convert the turn to Pi event data; some extension observations were skipped.`,
      next: 'Update octet or report this event with a sanitized reproduction. The committed turn has not been changed.' })));
  }
  // A handler outlived its budget and the hook stopped waiting for it. One
  // notice per extension and event; the handler itself keeps running. Each item
  // is [path, milliseconds the hook actually waited].
  slow(event, items) {
    this.publish(items.filter(([path]) => this.firstEventIssue(`slow:${event}`, path)).map(([path, milliseconds]) => ({ path,
      reason: `${display(event, 128)} did not finish within ${Math.round(milliseconds / 100) / 10} s; octet stopped waiting and continued without it (the extension keeps running).`,
      next: 'If its footer, status or prompt additions are missing, update or disable this extension.' })));
  }
  callback(event, factory, error) {
    const path = this.runtime.config.extensions[factory];
    if (!this.firstEventIssue(event, path)) return;
    const reason = error?.code === -32601
      ? `${display(event, 128)} callback needs an API or event field that octet does not support; the callback was skipped.`
      : `${display(event, 128)} callback failed and was skipped (private error details redacted).`;
    this.publish([{ path, reason,
      next: 'Update octet and the extension, or disable this extension, then restart octet. Report this path and event if it persists.' }]);
  }
}

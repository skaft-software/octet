import { basename, dirname, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { humanizeExtensionName, packageDisplayName } from './api.mjs';

// Extension problems reach the chat as at most one short notice per severity:
// the extension's name (never its path), what happened, and the one thing to
// do. Paths and every full reason go to the log (stderr). Callback notices
// additionally have a process-lifetime (extension, event) ledger: reporting a
// problem does not unsubscribe the handler or change any fail-closed boundary.
//
// Severity separates a problem that takes an extension away ('error': it was
// skipped) from one that only degrades it ('warning').

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

const stalledFor = text => /setup did not finish within [0-9.]+ s/.exec(String(text))?.[0];

// `reason` follows the extension's name ("<name>: <reason>.") and `next` is the
// one action. Neither contains a path, an error message or an identifier a user
// could not act on, and both read the same for one extension or several.
export function loadIssue(failure) {
  const { entry, code, name, error = '' } = failure;
  let reason, next;
  if (error.startsWith('invalid_request reviewed factory changed:')) {
    reason = 'skipped: changed since review';
    next = 'Review the change, then run configure.mjs --reviewed again.';
  } else if (code === 'ENOENT' || code === 'MODULE_NOT_FOUND' || code === 'ERR_MODULE_NOT_FOUND') {
    reason = 'skipped: a file is missing';
    next = 'Reinstall, or remove from your Pi settings.';
  } else if (name === 'FactoryStalled') {
    reason = `skipped: ${stalledFor(error) ?? 'still starting'}`;
    next = 'Check that its services are reachable, or disable.';
  } else if (code === 'EACCES' || code === 'EPERM') {
    reason = 'skipped: a file could not be read';
    next = 'Check file permissions, then restart octet.';
  } else if (code === -32030) {
    reason = 'skipped: needs Pi features octet does not provide';
    next = 'Disable, or run configure.mjs --reviewed --from-pi again.';
  } else if (code === -32031) {
    reason = 'skipped: needs a Pi install that is missing or unsupported';
    next = 'Install Pi 1.0.2 to 1.4.x, or disable.';
  } else if (error.startsWith('invalid_request duplicate registration ')) {
    reason = 'skipped: conflicts with another extension';
    next = 'Disable one of the two, then restart octet.';
  } else if (code === -32601) {
    reason = 'skipped: uses an API octet does not support';
    next = 'Update octet or the extension, or disable.';
  } else if (name === 'SyntaxError' || error.startsWith('invalid_request default export must be a factory:')) {
    reason = 'skipped: invalid source';
    next = 'Update or fix the extension.';
  } else {
    reason = `skipped: failed while loading${reportedCode(error) ? ` ("${reportedCode(error)}")` : ''}`;
    next = 'Check dependencies and settings, or disable.';
  }
  return { path: entry, level: 'error', reason, next };
}

// A factory left out of the mirror when the setup was captured: said once at
// start, with the one reason that is safe to show, never its raw error text.
export function excludedIssue({ entry, reason = '' }) {
  const code = reportedCode(reason);
  return { path: entry, level: 'error',
    reason: `left out: ${stalledFor(reason) ?? (code ? `failed with "${code}"` : 'could not be loaded')}`,
    next: 'Update or disable; octet tries again at the next start.' };
}

// The extension's own name: its package's, otherwise its file's (or its
// folder's, for an index file).
function nameOf(path) {
  try {
    const file = basename(String(path), extname(String(path)));
    return display(packageDisplayName(path) ?? humanizeExtensionName(file === 'index' ? basename(dirname(String(path))) : file), 64);
  } catch { return display(basename(String(path)), 64); }
}

const MAX_LINES = 4;

// Extensions with the same reason and action read as one line naming them all.
// What does not fit stays in the log, and the last line says so.
export function formatIssues(issues) {
  const groups = new Map();
  for (const issue of issues) {
    const key = JSON.stringify([issue.reason, issue.next]);
    const group = groups.get(key) ?? { names: [], issue };
    const name = nameOf(issue.path);
    if (!group.names.includes(name)) group.names.push(name);
    groups.set(key, group);
  }
  const lines = [...groups.values()].map(({ names, issue }) => {
    const shown = names.length > 3 ? `${names.slice(0, 3).join(', ')} and ${names.length - 3} more` : names.join(', ');
    return `${shown}: ${issue.reason}. ${issue.next}`;
  });
  if (lines.length > MAX_LINES) lines.splice(MAX_LINES - 1, lines.length, `${lines.length - MAX_LINES + 1} more problems. See /extensions for details.`);
  return lines;
}

const SEVERITIES = ['error', 'warning'];

export class ExtensionIssues {
  constructor(runtime) {
    this.runtime = runtime; this.started = false;
    this.callbacks = new Set(); this.renderers = new Set(); this.background = new Set();
    // Last extension-originated notice write, kept as an ordering barrier.
    this.pending = undefined;
  }
  publish(issues) {
    if (!issues.length) return;
    const runtime = this.runtime;
    for (const issue of issues) console.error(`[Extension issues] ${display(issue.path)}: ${issue.reason}. ${issue.next}`);
    if (runtime.stopping || runtime.transport.closed) return;
    // The host labels a negotiated source itself; without one the title carries it.
    const source = runtime.features.has('notification_source_v1') ? 'Pi extensions' : undefined;
    for (const level of SEVERITIES) {
      const group = issues.filter(issue => (issue.level ?? 'warning') === level);
      if (!group.length) continue;
      this.pending = runtime.transport.notify('notification', {
        level, title: '[Extension issues]', message: formatIssues(group).join('\n'), ...(source ? { source } : {}),
      }).catch(error => runtime.transport.fail(error));
    }
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
      issues.push({ path, level: 'warning',
        reason: 'custom display not shown; the default view is used',
        next: 'Disable if you need the custom display.' });
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
    this.publish([...paths].filter(path => this.firstEventIssue(event, path)).map(path => ({ path, level: 'warning',
      reason: `${display(event, 128)} data was unavailable, so some activity was missed`,
      next: 'Update octet if this keeps happening.' })));
  }
  // A handler outlived its budget and the hook stopped waiting for it. One
  // notice per extension and event; the handler itself keeps running. Each item
  // is [path, milliseconds the hook actually waited].
  slow(event, items) {
    this.publish(items.filter(([path]) => this.firstEventIssue(`slow:${event}`, path)).map(([path, milliseconds]) => ({ path, level: 'warning',
      reason: `${display(event, 128)} hook took over ${Math.round(milliseconds / 100) / 10} s and was skipped this time`,
      next: 'If status or prompt additions are missing, update or disable.' })));
  }
  callback(event, factory, error) {
    const path = this.runtime.config.extensions[factory];
    if (!this.firstEventIssue(event, path)) return;
    const reason = error?.code === -32601
      ? `${display(event, 128)} hook uses an event field octet does not support and was skipped`
      : `${display(event, 128)} hook failed and was skipped`;
    this.publish([{ path, level: 'warning', reason, next: 'Update or disable if this keeps happening.' }]);
  }
  // A failure with no extension to name. One line, once per distinct message;
  // the full text is in the log.
  backgroundError(error) {
    const full = String(error?.message || error).slice(0, 4096);
    console.error(`[pi-compat] ${full}`);
    const summary = display(full.split('\n', 1)[0].replace(/(?:file:\/\/)?\/(?:[^\s/'"]+\/)+/g, ''), 160);
    const runtime = this.runtime;
    if (this.background.has(summary) || this.background.size >= 32 || runtime.stopping || runtime.transport.closed) return;
    this.background.add(summary);
    runtime.transport.notify('notification', { level: 'error', title: 'Pi compatibility error', message: `${summary} (details: /extensions)` })
      .catch(failure => runtime.transport.fail(failure));
  }
}

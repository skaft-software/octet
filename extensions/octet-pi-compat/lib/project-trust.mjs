// Pi 1.5.0 trust-manager's no-project-resources rule plus its `project_trust`
// event (MIT; LICENSE.pi). Pi resolves project trust before it loads trusted
// project resources and notifies the extensions it already loaded: at startup
// before `session_start`, and again when a session replacement enters a cwd this
// process has not resolved yet (core/project-trust.ts:48-70,
// docs/extensions.md#project_trust). Only user/global and CLI extensions
// participate there; project-local extensions load after the decision.
//
// Octet decides workspace trust natively, before any Pi factory runs, and this
// bridge never changes that decision. So the event port keeps Pi's lifecycle
// point, payload, ordering and per-cwd cardinality, and drops Pi's *authority*:
// a handler's `{ trusted: "yes" | "no" }` is read and then discarded. It can
// neither grant nor revoke admission, and `remember` never writes a trust file.
// A decision that would have changed Pi's outcome is logged by name so the
// difference is never silent.
import { lstatSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { createContext } from './api.mjs';
import { facade } from './errors.mjs';

const projectResources = ['settings.json', 'mcp.json', 'extensions', 'skills', 'prompts', 'themes', 'SYSTEM.md', 'APPEND_SYSTEM.md'];
export const PROJECT_TRUST = 'project_trust';

function present(path) {
  try { lstatSync(path); return true; }
  catch (error) {
    return !['ENOENT', 'ENOTDIR'].includes(error.code);
  }
}

export function retainPiProjectTrust(state) {
  if (state.host.project_trusted === true) state.piPlainWorkspaceTrusted = false;
  else if (state.piPlainWorkspaceTrusted !== false) {
    state.piPlainWorkspaceTrusted = piProjectTrusted({ cwd: state.workspace, hostTrusted: false });
  }
}

export function piProjectTrusted({ cwd, hostTrusted, home = process.env.HOME || homedir() }) {
  if (hostTrusted) return true;
  let current, userSkills;
  try {
    current = realpathSync(cwd);
    userSkills = join(realpathSync(home), '.agents', 'skills');
  } catch { return false; }
  if (projectResources.some(entry => present(join(current, '.pi', entry)))) return false;
  for (;;) {
    const skills = join(current, '.agents', 'skills');
    if (skills !== userSkills && present(skills)) return false;
    const parent = dirname(current);
    if (parent === current) return true;
    current = parent;
  }
}

// Pi asks `hasTrustRequiringProjectResources` before resolving trust
// (trust-manager.ts:193-215, checked at project-trust.ts:49-51) and emits no
// event for a plain workspace. This is that question asked of the host-less
// case: an unreadable workspace counts as protected, which is the conservative
// reading and only ever adds a notification, never admission.
export function hasTrustRequiringProjectResources(cwd, home) {
  return !piProjectTrusted({ cwd, hostTrusted: false, ...(home === undefined ? {} : { home }) });
}

// Pi's handler decision, read for parity but never applied. Anything other than
// a positive "undecided" ends the loop as it does in Pi, where the first
// yes/no handler owns the decision and later handlers do not run.
function decision(value) {
  if (value === undefined || value === null) return 'undecided';
  if (typeof value !== 'object') return undefined;
  return value.trusted;
}

function reportUnapplied(runtime, factory, trusted, native) {
  const name = runtime.extensionNames.get(factory) ?? 'a Pi extension';
  runtime.diagnostic(`project_trust: ${name} answered "${trusted}" but octet decided workspace trust natively (${native ? 'trusted' : 'untrusted'}); native admission is unchanged`);
}

/// Deliver Pi's `project_trust` notification for this session's workspace, once
/// per workspace per process, immediately before `session_start` handlers run:
/// Pi's own order, and still before this bridge reads any Pi project resource.
/// The event carries Pi's `{ type, cwd }` payload from the host's own workspace;
/// the trust state a handler can observe is `ctx.isProjectTrusted()`, which reads
/// the native snapshot and never the handler's return value.
export async function projectTrustEvent(runtime, store) {
  const handlers = [...(runtime.events?.get(PROJECT_TRUST) ?? [])];
  if (!handlers.length) return;
  const state = store.state, native = state.host.project_trusted;
  // No authoritative native decision means no notification: a trust state is
  // never invented, and the event never stands in for one.
  if (typeof native !== 'boolean') {
    runtime.diagnostic('project_trust: no authoritative native workspace-trust snapshot; the event is not delivered');
    return;
  }
  const cwd = state.workspace;
  if (typeof cwd !== 'string' || !cwd) {
    runtime.diagnostic('project_trust: no authoritative native workspace; the event is not delivered');
    return;
  }
  // Pi resolves each cwd once per process (`projectTrustByCwd`), so a
  // replacement into a cwd already resolved in this process emits nothing.
  const resolved = runtime.projectTrustResolved ??= new Map();
  if (resolved.has(cwd)) return;
  // Recorded before dispatch: a handler that starts another session must not see
  // the same cwd resolve twice, and one workspace reports one trust state.
  resolved.set(cwd, native);
  if (!hasTrustRequiringProjectResources(cwd)) return;
  const event = facade({ type: PROJECT_TRUST, cwd }, 'project_trust event');
  for (const entry of handlers) {
    store.controller.signal.throwIfAborted();
    runtime.assertSessionOwner(store);
    const child = { ...store, factory: entry.factory };
    let value;
    try {
      value = await runtime.scope.run(child, () => entry.handler(event, createContext(runtime, child)));
    } catch (error) {
      store.controller.signal.throwIfAborted();
      runtime.assertSessionOwner(store);
      // Pi reports a failing project_trust handler and runs the next one.
      // Cancellation, owner refusals and tracked host mutations still propagate:
      // a skipped callback is never a host receipt.
      if (error?.code === -32800 || error?.code === -32002 || store.errors.includes(error)) throw error;
      runtime.reportCallbackError(PROJECT_TRUST, entry.factory, error);
      continue;
    }
    store.controller.signal.throwIfAborted();
    runtime.assertSessionOwner(store);
    const trusted = decision(value);
    if (trusted === 'undecided') continue;
    // Pi stops at the first yes/no. Octet stops too, so later handlers observe
    // what Pi would show them, and applies nothing of that decision.
    if (trusted !== 'yes' && trusted !== 'no') {
      runtime.diagnostic(`project_trust: ${runtime.extensionNames.get(entry.factory) ?? 'a Pi extension'} returned no valid decision; native workspace trust is unchanged`);
      return;
    }
    if ((trusted === 'yes') !== native) reportUnapplied(runtime, entry.factory, trusted, native);
    return;
  }
}

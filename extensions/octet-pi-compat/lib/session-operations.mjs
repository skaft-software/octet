// Awaited native Session operations, never advisory lifecycle notifications.
import { bounded, facade, fields, invalid, ownerKey, strict, unsupported } from './errors.mjs';
import { createContext } from './api.mjs';
import { canonicalToPi, cancellable } from './provider-context.mjs';
import { translateSessionEntries } from './session-mirror.mjs';
import { runCompactionCallback } from './compaction.mjs';
export const sessionReplacementHooks = ['session_before_switch', 'session_before_fork'];
export async function sessionReplacement(runtime, params, store) {
  const hook = params.hook, signal = store.controller.signal;
  if (!sessionReplacementHooks.includes(hook) || !runtime.metadata().hooks.includes(hook)) invalid('undeclared session replacement');
  ownerKey(params.context?.resource_owner); runtime.bind(params, store);
  const payload = params.payload;
  fields(payload, hook === 'session_before_switch' ? ['reason', 'targetSessionFile'] : ['entryId', 'position'], `${hook} event`);
  if (hook === 'session_before_switch') {
    if (!['new', 'resume'].includes(payload.reason)) invalid('session switch reason');
    if (payload.targetSessionFile !== undefined) bounded(payload.targetSessionFile, 'target session file', 4096);
  } else {
    bounded(payload.entryId, 'fork entry', 256);
    if (!['before', 'at'].includes(payload.position)) invalid('fork position');
  }
  return cancellable(runtime.queued(store, async () => {
    let disposition = { action: 'continue' };
    for (const entry of [...runtime.events.get(hook) || []]) {
      signal.throwIfAborted(); runtime.assertSessionOwner(store);
      const child = { ...store, factory: entry.factory };
      const result = await cancellable(runtime.scope.run(child, () => entry.handler(
        facade({ type: hook, ...payload }, `${hook} event`), createContext(runtime, child))), signal);
      signal.throwIfAborted();
      if (result === undefined) continue;
      fields(result, hook === 'session_before_switch' ? ['cancel'] : ['cancel', 'skipConversationRestore'], `${hook} result`);
      for (const name of ['cancel', 'skipConversationRestore']) if (result[name] !== undefined && typeof result[name] !== 'boolean') invalid(`${hook} ${name}`);
      // Pi 1.0.2 AgentSessionRuntime consumes cancel only; its before-fork
      // result does not propagate skipConversationRestore into replacement.
      if (result.cancel) { disposition = { action: 'deny', reason: 'Session replacement cancelled' }; break; }
    }
    await cancellable(runtime.flush(store), signal);
    return { disposition, context: [], notifications: [] };
  }), signal);
}

export const sessionOperationHooks = ['session_before_compact', 'session_compact', 'session_before_tree', 'session_tree'];
export async function sessionOperation(runtime, params, store) {
  runtime.require('session_entries'); const hook = params.hook;
  if (!sessionOperationHooks.includes(hook) || !runtime.metadata().hooks.includes(hook)) invalid('undeclared session operation');
  ownerKey(params.context?.resource_owner); runtime.bind(params, store, { requireLeaf: 'session operation' });
  store.providerContext = true;
  const body = params.payload, signal = store.controller.signal;
  if (hook === 'session_compact' && body?.kind === 'compaction_callback') {
    return cancellable(runtime.queued(store, () => runCompactionCallback(runtime, store, body)), signal);
  }
  const expected = { session_before_compact: 'before_compact', session_compact: 'compacted', session_before_tree: 'before_tree', session_tree: 'tree' }[hook];
  if (body?.kind !== expected) invalid('session operation kind');
  const live = () => { signal.throwIfAborted(); runtime.assertSessionOwner(store); };
  return cancellable(runtime.queued(store, async () => {
    live(); let event;
    if (hook === 'session_before_compact') {
      fields(body, ['kind', 'reason', 'first_kept', 'preparation', 'branch_entries', 'custom_instructions'], 'compaction operation');
      if (!['manual', 'threshold', 'overflow'].includes(body.reason)) invalid('compaction reason');
      const preparation = body.preparation;
      event = { type: hook, reason: body.reason, signal,
        ...(body.custom_instructions === null ? {} : { customInstructions: body.custom_instructions }),
        branchEntries: translateSessionEntries(body.branch_entries, runtime.namespace),
        preparation: strict({ firstKeptEntryId: body.first_kept, messagesToSummarize: canonicalToPi(preparation.messages),
          turnPrefixMessages: canonicalToPi(preparation.turn_prefix_messages), isSplitTurn: preparation.turn_prefix_messages.length > 0,
          ...(preparation.previous_summary === null ? {} : { previousSummary: preparation.previous_summary }) }, 'native compaction preparation') };
    } else if (hook === 'session_compact') {
      fields(body, ['kind', 'reason', 'entry', 'from_extension'], 'compaction committed');
      event = { type: hook, reason: body.reason, compactionEntry: translateSessionEntries([body.entry], runtime.namespace)[0], fromExtension: body.from_extension };
    } else if (hook === 'session_before_tree') {
      fields(body, ['kind', 'target_id', 'old_head', 'preparation'], 'tree preparation');
      const preparation = { targetId: body.target_id, oldLeafId: body.old_head };
      if (body.preparation !== undefined) {
        const native = body.preparation;
        fields(native, ['common_ancestor_id', 'entries_to_summarize', 'user_wants_summary', 'custom_instructions'], 'tree summary preparation');
        if (typeof native.user_wants_summary !== 'boolean') invalid('tree summary request');
        preparation.commonAncestorId = native.common_ancestor_id;
        preparation.entriesToSummarize = translateSessionEntries(native.entries_to_summarize, runtime.namespace);
        preparation.userWantsSummary = native.user_wants_summary;
        if (native.custom_instructions !== null) preparation.customInstructions = bounded(native.custom_instructions, 'tree summary instructions', 262144, { controls: true });
      }
      event = { type: hook, signal, preparation: strict(preparation, 'native tree preparation') };
    } else {
      fields(body, ['kind', 'old_head', 'new_head', 'summary_entry'], 'tree committed');
      event = { type: hook, newLeafId: body.new_head, oldLeafId: body.old_head };
      if (body.summary_entry !== undefined) {
        event.summaryEntry = translateSessionEntries([body.summary_entry], runtime.namespace)[0];
        if (event.summaryEntry.type !== 'branch_summary') invalid('tree committed summary entry');
        event.fromExtension = false;
      }
    }
    let decision = { action: 'continue' };
    for (const entry of [...runtime.events.get(hook) || []]) {
      live(); const child = { ...store, factory: entry.factory };
      const result = await cancellable(runtime.scope.run(child, () => entry.handler(facade({ ...event }, `${hook} event`), createContext(runtime, child))), signal);
      live();
      if (result === undefined) continue;
      if (!['session_before_compact', 'session_before_tree'].includes(hook)) unsupported(`${hook} result`, 'observation follows actual commit');
      fields(result, hook === 'session_before_compact' ? ['cancel', 'compaction'] : ['cancel'], `${hook} result`);
      if (result.cancel !== undefined && typeof result.cancel !== 'boolean') invalid('session cancellation boolean');
      if (result.cancel) { decision = { action: 'cancel' }; break; }
      if (result.compaction !== undefined) {
        fields(result.compaction, ['summary', 'firstKeptEntryId'], 'native compaction replacement');
        const summary = bounded(result.compaction.summary, 'compaction summary', 262144, { controls: true });
        const first_kept = bounded(result.compaction.firstKeptEntryId, 'compaction first kept entry', 256);
        if (!summary.trim() || !body.branch_entries.some(entry => entry.id === first_kept)) invalid('compaction replacement anchor');
        decision = { action: 'replace_compaction', replacement: { summary, first_kept } };
      }
    }
    await cancellable(runtime.flush(store), signal); live();
    return { disposition: { action: 'continue' }, context: [], notifications: [], session_operation: decision };
  }), signal);
}

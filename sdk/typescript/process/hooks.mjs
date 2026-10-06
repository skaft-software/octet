import {closed, text, uint} from './values.mjs';
import {json, own} from './schema.mjs';

export const HOOK_FEATURES = new Map([
  ...['before_prompt', 'after_response', 'before_tool_call', 'after_tool_call', 'provider_retry', 'before_persistence', 'post_mutation'].map(name => [name, null]),
  ['cache_warming_decision', 'cache_warming_decision'], ['compaction_strategy', 'compaction_strategy'],
  ...['model_turn_start', 'model_turn_end', 'session_before_compact', 'session_compact', 'session_before_tree', 'session_tree'].map(name => [name, 'session_entries']),
]);
export function hookResult(name, value) {
  value ??= {};
  const special = {provider_retry: 'provider_retry', before_persistence: 'persistence_metadata', post_mutation: 'post_mutation',
    cache_warming_decision: 'cache_warming_decision', compaction_strategy: 'compaction_frames'}[name];
  const session = HOOK_FEATURES.get(name) === 'session_entries';
  closed(value, [], ['disposition', 'context', 'notifications', ...(special ? [special] : []), ...(session ? ['session_operation'] : [])]);
  const result = {disposition: {action: 'continue'}, context: [], notifications: [], ...value};
  json(result);
  closed(result.disposition, ['action'], ['reason']);
  if (!['continue', 'deny'].includes(result.disposition.action)) throw new TypeError('Invalid hook disposition');
  if (result.disposition.action === 'deny') text(result.disposition.reason);
  if (!Array.isArray(result.context) || !Array.isArray(result.notifications)) throw new TypeError('Invalid hook contributions');
  for (const c of result.context) {
    closed(c, ['label', 'content', 'placement']); text(c.label); text(c.content, 65536, true);
    if (!['system_prefix', 'system_suffix', 'prompt_prefix', 'prompt_suffix'].includes(c.placement)) throw new TypeError('Invalid context placement');
  }
  for (const n of result.notifications) {
    closed(n, ['level', 'message'], ['title']); text(n.message);
    if (!['info', 'success', 'warning', 'error'].includes(n.level)) throw new TypeError('Invalid notification');
    if (n.title != null) text(n.title);
  }
  if (session) {
    if (result.context.length) throw new TypeError('Session observations cannot add prompt context');
    if (own(result, 'session_operation')) {
      const decision = result.session_operation;
      closed(decision, ['action'], decision.action === 'replace_compaction' ? ['replacement'] : []);
      if (!['continue', 'cancel', 'replace_compaction'].includes(decision.action)) throw new TypeError('Invalid session decision');
      if (decision.action === 'replace_compaction') {
        if (name !== 'session_before_compact') throw new TypeError('Replacement requires before compaction');
        closed(decision.replacement, ['summary', 'first_kept']);
        text(decision.replacement.summary, 262144); text(decision.replacement.first_kept, 256);
      }
    }
    if (['model_turn_start', 'model_turn_end', 'session_compact', 'session_tree'].includes(name) &&
        (result.disposition.action !== 'continue' || (result.session_operation?.action ?? 'continue') !== 'continue')) throw new TypeError('Observation requires continue');
  }
  if (special && own(result, special)) {
    const v = result[special];
    if (special === 'cache_warming_decision' && ![null, 'warm', 'stop'].includes(v)) throw new TypeError('Invalid warming decision');
    if (special === 'compaction_frames' && (!Array.isArray(v) || v.length < 1 || v.length > 32 || v.some(f => typeof f !== 'string' || Buffer.byteLength(f) > 524288))) throw new TypeError('Invalid compaction frames');
    if (special === 'provider_retry' && !['retry', 'stop'].includes(v)) {
      closed(v, ['delay']); closed(v.delay, ['additional_delay_ms']);
      if (!uint(v.delay.additional_delay_ms)) throw new TypeError('Invalid retry delay');
    }
    if (special === 'persistence_metadata') {
      closed(v, ['value'], ['public']); json(v.value, 16384);
      if (own(v, 'public') && typeof v.public !== 'boolean') throw new TypeError('Invalid metadata visibility');
    }
    if (special === 'post_mutation') {
      closed(v, ['action'], ['resource_ids']);
      if (!['no_rescan', 'request_rescan'].includes(v.action) || v.action === 'request_rescan' && (!Array.isArray(v.resource_ids) || !v.resource_ids.length || v.resource_ids.length > 32 || v.resource_ids.some(id => typeof id !== 'string'))) throw new TypeError('Invalid rescan');
    }
  }
  return result;
}

// Host-issued private append authority is single-use even on ambiguous failure.
export function sessionLeaf(value, owner, previous, head) {
  if (!value || !/^[a-f0-9]{64}$/.test(value.grant_id) || !uint(value.activation_epoch)) throw new TypeError('Invalid session leaf grant');
  text(value.operation_id, 256);
  const key = o => o && JSON.stringify([o.session_id, o.extension_instance_id, o.process_generation]);
  if (!owner || key(value.owner) !== key(owner)) throw new TypeError('Session leaf owner mismatch');
  if (value.expected_head !== null) text(value.expected_head, 256);
  if (previous && (value.activation_epoch !== previous.activation_epoch || value.operation_id !== previous.operation_id ||
      value.grant_id === previous.grant_id || value.expected_head !== head)) throw new TypeError('Session leaf successor mismatch');
  return JSON.parse(json(value));
}

// Only existing, active-parent services. No retained-owner, provider, or arbitrary RPC escape hatch.
export const SERVICES = new Map([
  ['confirmation/request', null], ['input/request', null], ['artifact/publish', 'artifacts'],
  ['policy/evaluate', 'policy_intents'], ['secret/get', 'secrets'],
  ...['composer/get', 'composer/set', 'composer/insert'].map(m => [m, 'composer']),
  ...['session/append_entry', 'session/set_name', 'session/set_label'].map(m => [m, 'session_entries']),
  ...['session/send_message', 'session/send_user_message'].map(m => [m, 'message_injection']),
  ['tools/set_active', 'active_tools'],
  ...['resource/register', 'resource/release'].map(m => [m, 'resource_refs_v1']),
  ...['bulk/write', 'bulk/commit', 'bulk/read', 'bulk/release'].map(m => [m, 'bulk_objects_v1']),
]);

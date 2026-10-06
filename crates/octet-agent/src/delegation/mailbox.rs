//! Bounded mailboxes, delivery pages and message size accounting.

use super::*;

pub(super) fn new_delivery_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("could not allocate delivery identity: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) fn delivery_ids_in_envelopes(text: &str) -> BTreeSet<String> {
    const OPEN: &str = "<octet_delegation_delivery id=\"";
    const MIDDLE: &str = "\" kind=\"";
    const CLOSE: &str = "</octet_delegation_delivery>";
    let mut ids = BTreeSet::new();
    let mut rest = text;
    while let Some(offset) = rest.find(OPEN) {
        rest = &rest[offset + OPEN.len()..];
        let Some((id, tail)) = rest.split_once(MIDDLE) else {
            break;
        };
        let Some((kind, tail)) = tail.split_once("\">\n") else {
            break;
        };
        let Some(close) = tail.find(CLOSE) else { break };
        if id.len() == 32
            && id.bytes().all(|byte| byte.is_ascii_hexdigit())
            && matches!(kind, "message" | "follow_up" | "initial")
        {
            ids.insert(id.to_owned());
        }
        rest = &tail[close + CLOSE.len()..];
    }
    ids
}

pub(super) fn format_initial_task(task: &str, pending: &[DirectedMessage]) -> String {
    if pending.is_empty() {
        return task.to_owned();
    }
    let mut formatted = String::new();
    for directed in pending {
        formatted.push_str(&format_direct_message(directed));
        formatted.push_str("\n\n");
    }
    formatted.push_str(task);
    formatted
}

pub(super) fn format_direct_message(message: &DirectedMessage) -> String {
    format!(
        "<octet_delegation_delivery id=\"{}\" kind=\"message\">\n<agent_message from=\"{}\">\n{}\n</agent_message>\n</octet_delegation_delivery>",
        message.delivery_id, message.from, message.message
    )
}

pub(super) fn format_follow_up(follow_up: &QueuedFollowUp, pending: &[DirectedMessage]) -> String {
    let mut formatted = String::new();
    for directed in pending {
        formatted.push_str(&format_direct_message(directed));
        formatted.push_str("\n\n");
    }
    formatted.push_str(&format!(
        "<octet_delegation_delivery id=\"{}\" kind=\"follow_up\">\n<followup_task from=\"{}\">\n{}\n</followup_task>\n</octet_delegation_delivery>",
        follow_up.delivery_id, follow_up.from, follow_up.message
    ));
    formatted
}

pub(super) fn mailbox_message_bytes(message: &MailboxMessage) -> usize {
    message.kind.len()
        + message.from.len()
        + message.task_name.as_ref().map_or(0, String::len)
        + message.message.len()
}

pub(super) fn mailbox_can_accept(
    mailbox: &VecDeque<MailboxMessage>,
    message: &MailboxMessage,
) -> bool {
    mailbox.len() < MAX_MAILBOX_MESSAGES
        && mailbox
            .iter()
            .fold(0usize, |total, item| {
                total.saturating_add(mailbox_message_bytes(item))
            })
            .saturating_add(mailbox_message_bytes(message))
            <= MAX_MAILBOX_BYTES
}

pub(super) fn mailbox_can_accept_after_evicting_automatic(
    mailbox: &VecDeque<MailboxMessage>,
    message: &MailboxMessage,
) -> bool {
    let message_bytes = mailbox_message_bytes(message);
    if message_bytes > MAX_MAILBOX_BYTES {
        return false;
    }
    let mut entries = mailbox.len();
    let mut bytes = mailbox.iter().fold(0usize, |total, item| {
        total.saturating_add(mailbox_message_bytes(item))
    });
    if entries < MAX_MAILBOX_MESSAGES && bytes.saturating_add(message_bytes) <= MAX_MAILBOX_BYTES {
        return true;
    }
    for entry in mailbox
        .iter()
        .filter(|entry| entry.evictable && !entry.leased)
    {
        entries = entries.saturating_sub(1);
        bytes = bytes.saturating_sub(mailbox_message_bytes(entry));
        if entries < MAX_MAILBOX_MESSAGES
            && bytes.saturating_add(message_bytes) <= MAX_MAILBOX_BYTES
        {
            return true;
        }
    }
    false
}

pub(super) fn push_mailbox_bounded(
    mailbox: &mut VecDeque<MailboxMessage>,
    message: MailboxMessage,
) {
    let message_bytes = mailbox_message_bytes(&message);
    if message_bytes > MAX_MAILBOX_BYTES {
        return;
    }
    while !mailbox_can_accept(mailbox, &message) {
        let Some(index) = mailbox
            .iter()
            .position(|entry| entry.evictable && !entry.leased)
        else {
            // Accepted direct messages are durable work and must never be evicted by
            // best-effort automatic notifications.
            return;
        };
        mailbox.remove(index);
    }
    mailbox.push_back(message);
}

pub(super) fn mailbox_delivery_message(
    message: &MailboxMessage,
    text: &str,
    remaining_bytes: usize,
) -> Value {
    let mut value = json!({
        "kind": message.kind,
        "from": message.from,
        "task_name": message.task_name,
        "message": text,
    });
    let object = value
        .as_object_mut()
        .expect("mailbox delivery message is an object");
    if message.continued {
        object.insert("continued".into(), Value::Bool(true));
    }
    if remaining_bytes > 0 {
        object.insert("remaining_bytes".into(), json!(remaining_bytes));
    }
    value
}

pub(super) fn mailbox_delivery_value(messages: Vec<Value>, more: bool) -> Value {
    json!({"timed_out": false, "messages": messages, "more": more})
}

pub(super) fn encoded_value_len(value: &Value) -> Result<usize, String> {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .map_err(|error| format!("could not encode mailbox delivery: {error}"))
}

pub(super) fn lease_mailbox_page(
    mailbox: &mut VecDeque<MailboxMessage>,
    delivery_id: u64,
    output_limit: usize,
) -> Result<(Value, MailboxDeliveryPlan), String> {
    if mailbox.iter().any(|message| message.leased) {
        return Err("a mailbox delivery is already awaiting durable acknowledgement".into());
    }

    let mut rendered = Vec::new();
    let mut complete_messages = 0usize;
    for message in mailbox.iter() {
        let mut candidate = rendered.clone();
        candidate.push(mailbox_delivery_message(message, &message.message, 0));
        // `false` is one byte longer than `true`, so this remains safe if
        // overflow means the final page needs to advertise `more: true`.
        if encoded_value_len(&mailbox_delivery_value(candidate, false))? > output_limit {
            break;
        }
        rendered.push(mailbox_delivery_message(message, &message.message, 0));
        complete_messages += 1;
    }

    let (value, partial_bytes, touched_messages) = if complete_messages > 0 {
        let more = complete_messages < mailbox.len();
        (mailbox_delivery_value(rendered, more), 0, complete_messages)
    } else {
        let message = mailbox
            .front()
            .expect("mailbox page is created only for a non-empty mailbox");
        let boundaries = message
            .message
            .char_indices()
            .map(|(index, _)| index)
            .skip(1)
            .filter(|index| *index < message.message.len())
            .collect::<Vec<_>>();
        let mut low = 0usize;
        let mut high = boundaries.len();
        let mut best = None;
        while low < high {
            let middle = low + (high - low) / 2;
            let end = boundaries[middle];
            let remaining = message.message.len() - end;
            let candidate = mailbox_delivery_value(
                vec![mailbox_delivery_message(
                    message,
                    &message.message[..end],
                    remaining,
                )],
                true,
            );
            if encoded_value_len(&candidate)? <= output_limit {
                best = Some((end, candidate));
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let Some((partial_bytes, value)) = best else {
            return Err(format!(
                "delegation tool-output limit ({output_limit} bytes) is too small for one mailbox message chunk"
            ));
        };
        (value, partial_bytes, 1)
    };

    debug_assert!(encoded_value_len(&value).is_ok_and(|length| length <= output_limit));
    for message in mailbox.iter_mut().take(touched_messages) {
        message.leased = true;
    }
    Ok((
        value,
        MailboxDeliveryPlan {
            id: delivery_id,
            complete_messages,
            partial_bytes,
            touched_messages,
        },
    ))
}

pub(super) fn resolve_mailbox_page(
    mailbox: &mut VecDeque<MailboxMessage>,
    plan: MailboxDeliveryPlan,
    delivered: bool,
) {
    if !delivered {
        for message in mailbox.iter_mut().take(plan.touched_messages) {
            message.leased = false;
        }
        return;
    }

    for _ in 0..plan.complete_messages {
        let removed = mailbox
            .pop_front()
            .expect("leased complete mailbox message still exists");
        debug_assert!(removed.leased);
    }
    if plan.partial_bytes > 0 {
        let message = mailbox
            .front_mut()
            .expect("leased partial mailbox message still exists");
        debug_assert!(message.leased && message.message.is_char_boundary(plan.partial_bytes));
        message.message.drain(..plan.partial_bytes);
        message.continued = true;
        message.leased = false;
    }
}

pub(super) fn push_mailbox_locked(state: &mut ManagerState, target: &str, message: MailboxMessage) {
    if target == ROOT_AGENT_ID {
        push_mailbox_bounded(&mut state.root_mailbox, message);
    } else if let Some(record) = state.records.get_mut(target) {
        push_mailbox_bounded(&mut record.mailbox, message);
    }
}

pub(super) fn directed_message_bytes(message: &DirectedMessage) -> usize {
    message.from.len().saturating_add(message.message.len())
}

pub(super) fn record_can_accept_pending_message(
    record: &AgentRecord,
    message: &DirectedMessage,
) -> bool {
    let pending_bytes = record.pending_messages.iter().fold(0usize, |total, item| {
        total.saturating_add(directed_message_bytes(item))
    });
    record
        .pending_messages
        .len()
        .saturating_add(record.reserved_messages.messages)
        .saturating_sub(record.inflight_message_ids.len())
        < MAX_PENDING_MESSAGES
        && pending_bytes
            .saturating_add(record.reserved_messages.bytes)
            .saturating_sub(
                record
                    .pending_messages
                    .iter()
                    .filter(|item| record.inflight_message_ids.contains(&item.delivery_id))
                    .map(directed_message_bytes)
                    .sum::<usize>(),
            )
            .saturating_add(directed_message_bytes(message))
            <= MAX_PENDING_MESSAGE_BYTES
}

pub(super) fn record_can_accept_follow_up(
    record: &AgentRecord,
    follow_up: &QueuedFollowUp,
) -> bool {
    let usage = follow_up.usage();
    record.queued_follow_ups.messages < MAX_QUEUED_FOLLOW_UPS
        && record.queued_follow_ups.bytes.saturating_add(usage.bytes) <= MAX_QUEUED_FOLLOW_UP_BYTES
}

pub(super) fn command_queue_error<T>(error: tokio::sync::mpsc::error::TrySendError<T>) -> String {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => {
            "target delegation command queue is full".into()
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            "target worker is no longer available".into()
        }
    }
}

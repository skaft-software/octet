//! Session-isolated, scalar-only events. No payload leaves the protocol writer.
use super::*;

/// A bounded bus owned by one host session, never a workspace-shared runtime.
/// Dropping or resetting its owner invalidates pending deliveries. The host,
/// not protocol arguments, supplies every process principal.
#[derive(Default)]
pub struct ExtensionEventBus {
    inner: StdMutex<BusState>,
}

struct BusState {
    binding_id: String,
    binding_revision: usize,
    topic_revision: usize,
    cancelled: CancellationToken,
    peers: BTreeMap<(String, u64), Peer>,
    topics: BTreeMap<String, Topic>,
}
impl Default for BusState {
    fn default() -> Self {
        Self {
            binding_id: new_extension_instance_id(),
            binding_revision: 1,
            topic_revision: 0,
            cancelled: CancellationToken::default(),
            peers: BTreeMap::new(),
            topics: BTreeMap::new(),
        }
    }
}
struct Peer {
    termination: Option<ProcessTerminationHandle>,
    cancelled: CancellationToken,
    interests: BTreeSet<String>,
    writer: mpsc::Sender<WriterFrame>,
    frame_limit: Arc<ProtocolFrameLimit>,
    closed: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    subscriptions: BTreeMap<String, CancellationToken>,
    sequence: usize,
    budget: Arc<Budget>,
}
#[derive(Default)]
struct Budget {
    messages: AtomicUsize,
    bytes: AtomicUsize,
}
struct Topic {
    revision: usize,
    owner: (String, u64),
    fields: Vec<api_v03::BusFieldSpec>,
    cancelled: CancellationToken,
}

/// Queue credit is retained through the physical write, not merely dequeue.
/// Cancellation/expiry discard queued frames. If writing has already started,
/// they fail the stream closed rather than completing a stale partial frame.
pub(super) struct Delivery {
    budget: Arc<Budget>,
    bytes: usize,
    topic_cancelled: CancellationToken,
    subscription_cancelled: CancellationToken,
    closed: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    queued: Instant,
    control: bool,
}
impl Delivery {
    pub(super) fn is_current(&self) -> bool {
        !self.topic_cancelled.is_cancelled()
            && !self.subscription_cancelled.is_cancelled()
            && !self.closed.load(Ordering::Acquire)
            && !self.draining.load(Ordering::Acquire)
            && self.queued.elapsed()
                <= Duration::from_millis(api_v03::MAX_BUS_MESSAGE_AGE_MS as u64)
    }

    pub(super) fn control_expired(&self) -> bool {
        self.control
            && !self.topic_cancelled.is_cancelled()
            && !self.subscription_cancelled.is_cancelled()
            && self.queued.elapsed() > Duration::from_millis(api_v03::MAX_BUS_MESSAGE_AGE_MS as u64)
    }

    /// Cancellation of a partial frame must close the protocol stream: it can
    /// never be completed later in a replacement session or replayed. Already
    /// written bytes cannot be rolled back, just like other protocol writes.
    pub(super) async fn guard_write(
        &self,
        write: impl std::future::Future<Output = Result<(), String>>,
    ) -> Result<(), String> {
        if !self.is_current() {
            return Err("bus delivery invalidated before write".into());
        }
        let remaining = Duration::from_millis(api_v03::MAX_BUS_MESSAGE_AGE_MS as u64)
            .saturating_sub(self.queued.elapsed());
        tokio::select! {
            biased;
            _ = self.topic_cancelled.cancelled() => Err("bus publisher/session invalidated during write".into()),
            _ = self.subscription_cancelled.cancelled() => Err("bus subscription invalidated during write".into()),
            _ = tokio::time::sleep(remaining) => Err("bus delivery expired during write".into()),
            result = write => result,
        }
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        self.budget.messages.fetch_sub(1, Ordering::AcqRel);
        self.budget.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
impl Drop for Topic {
    fn drop(&mut self) {
        self.cancelled.cancel();
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.cancelled.cancel();
        for live in self.subscriptions.values() {
            live.cancel();
        }
    }
}

fn error(name: &str) -> api_v03::ContractError {
    let spec = api_v03::error_spec(name).expect("known bus error");
    api_v03::ContractError {
        code: spec.code,
        message: spec.message.to_owned(),
    }
}
fn invalid() -> api_v03::ContractError {
    error("invalid_params")
}
fn exhausted() -> api_v03::ContractError {
    error("resource_exhausted")
}
fn denied() -> api_v03::ContractError {
    error("capability_mismatch")
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= api_v03::MAX_BUS_IDENTIFIER_BYTES
        && value
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}
fn topic_owner(topic: &str) -> Result<&str, api_v03::ContractError> {
    let segments: Vec<_> = topic.split('.').collect();
    if topic.len() > api_v03::MAX_BUS_TOPIC_BYTES
        || segments.len() != 3
        || segments[0] != "bus"
        || !identifier(segments[1])
        || !identifier(segments[2])
    {
        return Err(invalid());
    }
    Ok(segments[1])
}
fn forbidden_field(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    [
        "apikey",
        "authorization",
        "capability",
        "credential",
        "cookie",
        "handle",
        "keychain",
        "oauth",
        "passwd",
        "password",
        "path",
        "pem",
        "private",
        "secret",
        "session",
        "token",
        "trust",
    ]
    .iter()
    .any(|word| normalized.contains(word))
}
fn screen_string(value: &str, max_bytes: usize) -> Result<(), api_v03::ContractError> {
    if value.len() > max_bytes.min(api_v03::MAX_BUS_STRING_BYTES) {
        return Err(exhausted());
    }
    if value.chars().any(char::is_control) {
        return Err(invalid());
    }
    let text = value.trim();
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    // Conservative bounded screening, not a PII classifier. Reject at least the
    // SDK deny-list, including enum values (not just free-form strings).
    let private_path = text.starts_with('/')
        || text.starts_with("~/")
        || text.starts_with("~\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'));
    let secret = [
        "sk-",
        "sk_",
        "pk-",
        "pk_",
        "rk-",
        "rk_",
        "ghp-",
        "ghp_",
        "gho-",
        "gho_",
        "ghu-",
        "ghu_",
        "ghs-",
        "ghs_",
        "github_pat-",
        "github_pat_",
        "xoxa-",
        "xoxa_",
        "xoxb-",
        "xoxb_",
        "xoxp-",
        "xoxp_",
        "xoxr-",
        "xoxr_",
        "xoxs-",
        "xoxs_",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix) && lower.len() >= prefix.len() + 8);
    let phone = text.len() >= 8
        && text
            .trim_start_matches('+')
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
        && text
            .bytes()
            .all(|c| c.is_ascii_digit() || b"+ ()-.".contains(&c))
        && text.bytes().any(|c| b" ()-.".contains(&c));
    if private_path
        || secret
        || phone
        || text.contains('@')
        || lower.starts_with("-----begin")
        || lower
            .strip_prefix("bearer")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    {
        return Err(invalid());
    }
    Ok(())
}
fn validate_fields(fields: &[api_v03::BusFieldSpec]) -> Result<(), api_v03::ContractError> {
    let mut seen = BTreeSet::new();
    for field in fields {
        if !identifier(&field.name)
            || forbidden_field(&field.name)
            || !seen.insert(&field.name)
            || field.max_bytes == 0
            || field.max_bytes > api_v03::MAX_BUS_STRING_BYTES
            || field
                .minimum
                .zip(field.maximum)
                .is_some_and(|(min, max)| min > max)
            || (field.kind != "integer" && (field.minimum.is_some() || field.maximum.is_some()))
            || (field.kind != "enum" && !field.values.is_empty())
            || (field.kind == "enum" && field.values.is_empty())
        {
            return Err(invalid());
        }
        let mut values = BTreeSet::new();
        for value in &field.values {
            screen_string(value, field.max_bytes)?;
            if !values.insert(value) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn validate_payload(
    fields: &[api_v03::BusFieldSpec],
    value: &serde_json::Value,
) -> Result<(), api_v03::ContractError> {
    let object = value.as_object().ok_or_else(invalid)?;
    if object.len() > api_v03::MAX_BUS_FIELDS {
        return Err(exhausted());
    }
    for (key, value) in object {
        let field = fields
            .iter()
            .find(|field| field.name == *key)
            .ok_or_else(invalid)?;
        match field.kind.as_str() {
            "string" | "enum" => {
                let text = value.as_str().ok_or_else(invalid)?;
                screen_string(text, field.max_bytes)?;
                if field.kind == "enum" && !field.values.iter().any(|allowed| allowed == text) {
                    return Err(invalid());
                }
            }
            "integer" => {
                let number = value.as_i64().ok_or_else(invalid)?;
                if field.minimum.is_some_and(|min| number < min)
                    || field.maximum.is_some_and(|max| number > max)
                {
                    return Err(invalid());
                }
            }
            "boolean" if value.is_boolean() => (),
            _ => return Err(invalid()),
        }
    }
    if fields
        .iter()
        .any(|field| field.required && !object.contains_key(&field.name))
    {
        return Err(invalid());
    }
    Ok(())
}

impl Peer {
    fn fail_closed(&self) {
        self.closed.store(true, Ordering::Release);
        self.cancelled.cancel();
        for live in self.subscriptions.values() {
            live.cancel();
        }
        if let Some(termination) = self.termination.clone() {
            termination.terminate();
        }
    }

    // Control messages share physical writer slots and bus credits with data.
    // Losing a current control message would leave an apparently live stale
    // consumer; pressure therefore retires this process, never silently drops.
    fn control(&self, binding: &CancellationToken, params: serde_json::Value) {
        if self.closed.load(Ordering::Acquire) || self.draining.load(Ordering::Acquire) {
            return;
        }
        let frame = serde_json::json!({"jsonrpc":"2.0","method":"bus/lifecycle","params":params});
        let queued = (|| {
            let mut line = api_v03::canonical_frame(&frame, api_v03::MAX_BUS_MESSAGE_BYTES)
                .ok()?
                .into_bytes();
            line.push(b'\n');
            if !self.frame_limit.accepts_message_bytes(line.len())
                || self.budget.messages.load(Ordering::Acquire) >= api_v03::MAX_BUS_QUEUE_MESSAGES
                || self.budget.bytes.load(Ordering::Acquire) + line.len()
                    > api_v03::MAX_BUS_QUEUE_BYTES
            {
                return None;
            }
            let permit = self.writer.clone().try_reserve_owned().ok()?;
            self.budget.messages.fetch_add(1, Ordering::AcqRel);
            self.budget.bytes.fetch_add(line.len(), Ordering::AcqRel);
            let bytes = line.len();
            permit.send(WriterFrame {
                line,
                state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                completion: None,
                bus_delivery: Some(Delivery {
                    budget: self.budget.clone(),
                    bytes,
                    topic_cancelled: binding.clone(),
                    subscription_cancelled: self.cancelled.clone(),
                    closed: self.closed.clone(),
                    draining: self.draining.clone(),
                    queued: Instant::now(),
                    control: true,
                }),
            });
            Some(())
        })();
        if queued.is_none() {
            self.fail_closed();
        }
    }
}

impl ExtensionEventBus {
    /// Attach only after initialization/cutover, before reading any further
    /// child bytes. A request never implicitly registers a process.
    pub(super) fn attach(&self, reader: &ProtocolReadState) -> Result<(), api_v03::ContractError> {
        let contract = read_std_lock(&reader.api_v03_contract);
        let Some(contract) = contract
            .as_ref()
            .filter(|contract| contract.capabilities.contains("event_bus"))
        else {
            return Ok(());
        };
        api_v03::require_method(
            contract,
            "bus/lifecycle",
            api_v03::MethodDirection::HostToExtension,
        )
        .map_err(|_| denied())?;
        if !identifier(&reader.extension_identity.name) {
            return Err(invalid());
        }
        let mut state = lock_std_mutex(&self.inner);
        let key = (reader.instance_id.clone(), reader.generation);
        if state.peers.contains_key(&key) {
            return Ok(());
        }
        if state.peers.len() >= api_v03::MAX_BUS_PEERS {
            return Err(exhausted());
        }
        let peer = Peer {
            writer: reader.writer.clone(),
            frame_limit: reader.frame_limit.clone(),
            closed: reader.closed.clone(),
            draining: reader.draining.clone(),
            termination: reader.termination.clone(),
            cancelled: CancellationToken::default(),
            interests: BTreeSet::new(),
            subscriptions: BTreeMap::new(),
            sequence: 0,
            budget: Arc::new(Budget::default()),
        };
        peer.control(&state.cancelled, serde_json::json!({"kind":"binding","binding_id":state.binding_id,"binding_revision":state.binding_revision}));
        if peer.closed.load(Ordering::Acquire) {
            return Err(exhausted());
        }
        state.peers.insert(key, peer);
        Ok(())
    }

    /// Replace the bus incarnation, preserving peers and outstanding credits.
    /// Mandatory lifecycle consumers explicitly rebind; publications never replay.
    pub fn reset(&self) {
        let mut state = lock_std_mutex(&self.inner);
        state.cancelled.cancel();
        state.cancelled = CancellationToken::default();
        state.binding_id = new_extension_instance_id();
        state.binding_revision += 1;
        state.topics.clear();
        let notification = serde_json::json!({"kind":"binding","binding_id":state.binding_id,"binding_revision":state.binding_revision});
        let cancelled = state.cancelled.clone();
        for peer in state.peers.values_mut() {
            for live in peer.subscriptions.values() {
                live.cancel();
            }
            peer.subscriptions.clear();
            peer.interests.clear();
            peer.control(&cancelled, notification.clone());
        }
    }

    pub(super) fn remove(&self, instance: &str, generation: u64) {
        let mut state = lock_std_mutex(&self.inner);
        let key = (instance.to_owned(), generation);
        state.peers.remove(&key);
        let removed: Vec<_> = state
            .topics
            .iter()
            .filter(|(_, topic)| topic.owner == key)
            .map(|(name, _)| name.clone())
            .collect();
        for name in removed {
            state.topics.remove(&name);
            state.topic_revision += 1;
            let notification = serde_json::json!({"kind":"topic_unavailable","binding_id":state.binding_id,"topic":name,
                "topic_revision":state.topic_revision,"publisher_instance_id":instance,"process_generation":generation});
            let cancelled = state.cancelled.clone();
            for peer in state.peers.values_mut() {
                if let Some(live) = peer.subscriptions.remove(&name) {
                    live.cancel();
                }
                // Interest survives publisher replacement, not session reset.
                if peer.interests.contains(&name) {
                    peer.control(&cancelled, notification.clone());
                }
            }
        }
    }

    /// Admit the reply on the same writer before releasing the mutation lock.
    /// A concurrent publisher or lifecycle transition cannot overtake a fresh
    /// subscription ACK. `respond` must synchronously attempt bounded admission,
    /// never await the physical write or re-enter the bus.
    pub(super) fn dispatch_with_response(
        &self,
        reader: &ProtocolReadState,
        method: &str,
        params: serde_json::Value,
        respond: impl FnOnce(Result<serde_json::Value, api_v03::ContractError>) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = lock_std_mutex(&self.inner);
        let result = Self::dispatch_locked(&mut state, reader, method, params);
        let queued = respond(result);
        if queued.is_err() {
            // An unacknowledged mutation must not leave a seemingly live peer
            // that can receive events after its response writer was exhausted.
            let key = (reader.instance_id.clone(), reader.generation);
            if let Some(peer) = state.peers.get(&key) {
                peer.fail_closed();
            }
        }
        queued
    }

    #[cfg(test)]
    pub(super) fn dispatch(
        &self,
        reader: &ProtocolReadState,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, api_v03::ContractError> {
        Self::dispatch_locked(&mut lock_std_mutex(&self.inner), reader, method, params)
    }

    fn dispatch_locked(
        state: &mut BusState,
        reader: &ProtocolReadState,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, api_v03::ContractError> {
        if reader.closed.load(Ordering::Acquire) || reader.draining.load(Ordering::Acquire) {
            return Err(denied());
        }
        let key = (reader.instance_id.clone(), reader.generation);
        // Validate the child-captured incarnation under the mutation lock. Even
        // bytes buffered before reset but read after a new declaration refuse.
        if !state.peers.contains_key(&key) {
            return Err(denied());
        }
        match params.get("binding_id").and_then(serde_json::Value::as_str) {
            Some(binding)
                if !binding.is_empty() && binding.len() <= api_v03::MAX_SESSION_HOOK_ID_BYTES =>
            {
                if binding != state.binding_id {
                    return Err(denied());
                }
            }
            _ => return Err(invalid()),
        }
        match method {
            "bus/declare" => {
                let request = api_v03::parse_bus_declare_params(params)?;
                if topic_owner(&request.topic)? != reader.extension_identity.name {
                    return Err(denied());
                }
                validate_fields(&request.fields)?;
                if state.topics.contains_key(&request.topic) {
                    return Err(invalid());
                }
                if state.topics.len() >= api_v03::MAX_BUS_TOPICS {
                    return Err(exhausted());
                }
                state.topic_revision += 1;
                let revision = state.topic_revision;
                state.topics.insert(
                    request.topic.clone(),
                    Topic {
                        revision,
                        owner: key,
                        fields: request.fields,
                        cancelled: CancellationToken::default(),
                    },
                );
                let notification = serde_json::json!({"kind":"topic_available","binding_id":state.binding_id,"topic":request.topic,
                    "topic_revision":revision,"publisher_instance_id":reader.instance_id,"process_generation":reader.generation});
                for peer in state
                    .peers
                    .values()
                    .filter(|peer| peer.interests.contains(&request.topic))
                {
                    peer.control(&state.cancelled, notification.clone());
                }
                Ok(serde_json::json!({"binding_id":state.binding_id}))
            }
            "bus/subscribe" | "bus/unsubscribe" => {
                let request = api_v03::parse_bus_topic_params(params)?;
                topic_owner(&request.topic)?;
                // A subscribe cannot implicitly negotiate the delivery method.
                if method == "bus/subscribe" {
                    let contract = read_std_lock(&reader.api_v03_contract);
                    api_v03::require_method(
                        contract.as_ref().ok_or_else(denied)?,
                        "bus/event",
                        api_v03::MethodDirection::HostToExtension,
                    )
                    .map_err(|_| denied())?;
                }
                let active = state
                    .topics
                    .get(&request.topic)
                    .map(|topic| (topic.owner.clone(), topic.revision));
                let binding_id = state.binding_id.clone();
                let revision = state.topic_revision;
                let peer = state.peers.get_mut(&key).expect("registered peer");
                if method == "bus/unsubscribe" {
                    peer.interests.remove(&request.topic);
                    if let Some(live) = peer.subscriptions.remove(&request.topic) {
                        live.cancel();
                    }
                    return Ok(serde_json::json!({"binding_id":binding_id}));
                }
                if !peer.interests.contains(&request.topic)
                    && peer.interests.len() >= api_v03::MAX_BUS_SUBSCRIPTIONS
                {
                    return Err(exhausted());
                }
                peer.interests.insert(request.topic.clone());
                if let Some(((instance, generation), topic_revision)) = active {
                    peer.subscriptions.entry(request.topic).or_default();
                    Ok(
                        serde_json::json!({"state":"active","binding_id":binding_id,"topic_revision":topic_revision,
                        "publisher_instance_id":instance,"process_generation":generation}),
                    )
                } else {
                    // Explicit successful bounded interest admission, NOT an
                    // active subscription. A later availability notice requires
                    // a fresh subscribe request and host-provenance ACK.
                    Ok(
                        serde_json::json!({"state":"pending","binding_id":binding_id,"topic_revision":revision}),
                    )
                }
            }
            "bus/publish" => {
                let request = api_v03::parse_bus_publish_params(params)?;
                topic_owner(&request.topic)?;
                let topic = state.topics.get(&request.topic).ok_or_else(invalid)?;
                if topic.owner != key {
                    return Err(denied());
                }
                validate_payload(&topic.fields, &request.payload)?;
                // A process-wide counter stays monotonic even when an active
                // session reset removes and later redeclares the same topic.
                let sequence = state
                    .peers
                    .get(&key)
                    .expect("registered publisher")
                    .sequence
                    .checked_add(1)
                    .filter(|n| *n <= api_v03::MAX_PORTABLE_JSON_INTEGER as usize)
                    .ok_or_else(exhausted)?;
                let published_at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| invalid())?
                    .as_millis()
                    .min(api_v03::MAX_PORTABLE_JSON_INTEGER as u128)
                    as usize;
                let event = api_v03::BusEventParams {
                    binding_id: state.binding_id.clone(),
                    topic: request.topic.clone(),
                    publisher: reader.extension_identity.name.clone(),
                    publisher_instance_id: reader.instance_id.clone(),
                    process_generation: reader.generation as usize,
                    sequence,
                    published_at_ms,
                    payload: request.payload,
                };
                let params = serde_json::to_value(event).map_err(|_| invalid())?;
                api_v03::parse_bus_event_params(params.clone())?;
                let frame =
                    serde_json::json!({"jsonrpc":"2.0", "method":"bus/event", "params": params});
                let mut line =
                    api_v03::canonical_frame(&frame, api_v03::MAX_BUS_MESSAGE_BYTES)?.into_bytes();
                line.push(b'\n');
                // Reserve every recipient's physical writer and byte/message
                // credit before committing any event. Failure never partially
                // fans out or consumes a publisher sequence.
                let mut deliveries = Vec::new();
                for peer in state.peers.values() {
                    let Some(subscribed) = peer.subscriptions.get(&request.topic) else {
                        continue;
                    };
                    if peer.closed.load(Ordering::Acquire) || peer.draining.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    if !peer.frame_limit.accepts_message_bytes(line.len())
                        || peer.budget.messages.load(Ordering::Acquire)
                            >= api_v03::MAX_BUS_QUEUE_MESSAGES
                        || peer.budget.bytes.load(Ordering::Acquire) + line.len()
                            > api_v03::MAX_BUS_QUEUE_BYTES
                    {
                        return Err(exhausted());
                    }
                    let permit = peer
                        .writer
                        .clone()
                        .try_reserve_owned()
                        .map_err(|_| exhausted())?;
                    deliveries.push((
                        permit,
                        peer.budget.clone(),
                        subscribed.clone(),
                        peer.closed.clone(),
                        peer.draining.clone(),
                    ));
                }
                for (permit, budget, subscribed, closed, draining) in deliveries {
                    budget.messages.fetch_add(1, Ordering::AcqRel);
                    budget.bytes.fetch_add(line.len(), Ordering::AcqRel);
                    permit.send(WriterFrame {
                        line: line.clone(),
                        state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
                        completion: None,
                        bus_delivery: Some(Delivery {
                            budget,
                            bytes: line.len(),
                            topic_cancelled: topic.cancelled.clone(),
                            subscription_cancelled: subscribed,
                            closed,
                            draining,
                            queued: Instant::now(),
                            control: false,
                        }),
                    });
                }
                state
                    .peers
                    .get_mut(&key)
                    .expect("registered publisher")
                    .sequence = sequence;
                Ok(
                    serde_json::json!({"binding_id":state.binding_id,"sequence":sequence, "published_at_ms":published_at_ms}),
                )
            }
            _ => Err(error("unknown_method")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn peer(name: &str, capacity: usize) -> (ProtocolReadState, mpsc::Receiver<WriterFrame>) {
        let (events, _) = broadcast::channel(8);
        let (mut reader, _) = super::super::tests::protocol_read_state_for_test(
            ManifestContributions::default(),
            events,
        );
        let (writer, frames) = mpsc::channel(capacity);
        reader.writer = writer;
        reader.instance_id = format!("instance-{name}");
        reader.extension_identity.name = name.into();
        reader.frame_limit = Arc::new(ProtocolFrameLimit::new(16385, true));
        let offer = api_v03_host_offer_for_services(16384, 4, false, true).unwrap();
        let mut selection = api_v03::select_required(&offer).unwrap();
        selection.capabilities.push("event_bus".into());
        selection.methods.extend(
            offer
                .optional_methods
                .iter()
                .filter(|method| method.starts_with("bus/"))
                .cloned(),
        );
        *write_std_lock(&reader.api_v03_contract) =
            Some(api_v03::negotiate(&offer, &selection).unwrap());
        (reader, frames)
    }

    /// One attached participant. Control notifications share the physical
    /// writer channel with data, so the wrapper reads them transparently and
    /// keeps every assertion about measured data/credits unchanged.
    struct Participant {
        reader: Arc<ProtocolReadState>,
        frames: mpsc::Receiver<WriterFrame>,
    }

    fn attached(bus: &ExtensionEventBus, name: &str, capacity: usize) -> Participant {
        let (reader, frames) = peer(name, capacity);
        let reader = Arc::new(reader);
        bus.attach(&reader).unwrap();
        let mut participant = Participant { reader, frames };
        participant.drain_control();
        participant
    }

    impl Participant {
        fn control_only(&self, frame: &WriterFrame) -> bool {
            frame
                .bus_delivery
                .as_ref()
                .is_some_and(|delivery| delivery.control)
        }

        /// Discard queued control notifications. Used before tests that measure
        /// an exact data backlog so controls cannot occupy a credit slot.
        fn drain_control(&mut self) {
            loop {
                match self.frames.try_recv() {
                    Ok(frame) => {
                        assert!(
                            self.control_only(&frame),
                            "unexpected data frame while draining control"
                        );
                    }
                    Err(_) => return,
                }
            }
        }

        fn data(&mut self) -> Option<WriterFrame> {
            loop {
                let frame = self.frames.try_recv().ok()?;
                if !self.control_only(&frame) {
                    return Some(frame);
                }
            }
        }

        fn expect_data(&mut self) -> WriterFrame {
            self.data().expect("queued data frame")
        }

        fn no_data(&mut self) -> bool {
            self.data().is_none()
        }
    }

    /// The host-issued incarnation every request must currently capture.
    fn binding(bus: &ExtensionEventBus) -> String {
        lock_std_mutex(&bus.inner).binding_id.clone()
    }

    fn declare(bus: &ExtensionEventBus, publisher: &ProtocolReadState) {
        let binding = binding(bus);
        bus.dispatch(
            publisher,
            "bus/declare",
            json!({"binding_id":binding,"topic":"bus.alpha.status","fields":[
                {"name":"summary","kind":"string","required":true,"max_bytes":1024,"values":[]}
            ]}),
        )
        .unwrap();
    }
    fn publish(
        bus: &ExtensionEventBus,
        publisher: &ProtocolReadState,
    ) -> Result<serde_json::Value, api_v03::ContractError> {
        let binding = binding(bus);
        bus.dispatch(
            publisher,
            "bus/publish",
            json!({"binding_id":binding,"topic":"bus.alpha.status","payload":{"summary":"safe"}}),
        )
    }
    fn subscribe(bus: &ExtensionEventBus, peer: &ProtocolReadState) {
        let binding = binding(bus);
        bus.dispatch(
            peer,
            "bus/subscribe",
            json!({"binding_id":binding,"topic":"bus.alpha.status"}),
        )
        .unwrap_or_else(|error| {
            panic!(
                "subscribe refused: {error:?}; negotiated={:?}",
                read_std_lock(&peer.api_v03_contract)
                    .as_ref()
                    .map(|contract| (contract.capabilities.clone(), contract.methods.clone()))
            )
        });
    }
    fn unsubscribe(bus: &ExtensionEventBus, peer: &ProtocolReadState) {
        let binding = binding(bus);
        bus.dispatch(
            peer,
            "bus/unsubscribe",
            json!({"binding_id":binding,"topic":"bus.alpha.status"}),
        )
        .unwrap();
    }

    #[test]
    fn subscription_ack_precedes_concurrent_publisher_on_the_physical_writer() {
        let bus = Arc::new(ExtensionEventBus::default());
        let alpha = attached(&bus, "alpha", 8);
        let mut beta = attached(&bus, "beta", 8);
        declare(&bus, &alpha.reader);
        let incarnation = binding(&bus);
        let publication = json!({"binding_id":incarnation,"topic":"bus.alpha.status","payload":{"summary":"safe"}});
        let (start, started) = std::sync::mpsc::channel();
        let (attempted, attempt) = std::sync::mpsc::channel();
        let publisher_bus = bus.clone();
        let publisher = std::thread::spawn(move || {
            started.recv_timeout(Duration::from_secs(2)).unwrap();
            // The subscriber is already active, but its reply has not yet been
            // admitted. A publisher on another protocol reader must be fenced.
            assert!(matches!(
                publisher_bus.inner.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            attempted.send(()).unwrap();
            publisher_bus.dispatch(&alpha.reader, "bus/publish", publication)
        });
        let id = ExtensionRequestId::String("subscribe".into());
        insert_child_request(&beta.reader, id.clone(), None, None).unwrap();
        bus.dispatch_with_response(
            &beta.reader,
            "bus/subscribe",
            json!({"binding_id":incarnation,"topic":"bus.alpha.status"}),
            |result| {
                assert_eq!(result.as_ref().unwrap()["state"], "active");
                start.send(()).unwrap();
                attempt.recv_timeout(Duration::from_secs(2)).unwrap();
                queue_provider_host_response(&beta.reader, &id, Ok(result.unwrap()))
            },
        )
        .unwrap();
        assert_eq!(publisher.join().unwrap().unwrap()["sequence"], 1);
        let ack = beta.frames.try_recv().unwrap();
        assert!(ack.bus_delivery.is_none());
        let ack: serde_json::Value = serde_json::from_slice(&ack.line).unwrap();
        assert_eq!(ack["id"], "subscribe");
        assert_eq!(ack["result"]["state"], "active");
        let event = beta.expect_data();
        assert!(event.bus_delivery.as_ref().unwrap().is_current());
        let event: serde_json::Value = serde_json::from_slice(&event.line).unwrap();
        assert_eq!(event["method"], "bus/event");
        assert_eq!(event["params"]["sequence"], 1);
        assert!(lock_std_mutex(&beta.reader.child_requests).is_empty());
    }

    #[test]
    fn subscription_ack_writer_pressure_retires_the_unacknowledged_peer() {
        let bus = ExtensionEventBus::default();
        let alpha = attached(&bus, "alpha", 8);
        let mut beta = attached(&bus, "beta", 1);
        declare(&bus, &alpha.reader);
        let id = ExtensionRequestId::String("subscribe".into());
        insert_child_request(&beta.reader, id.clone(), None, None).unwrap();
        queue_writer_value(
            &beta.reader.writer,
            &beta.reader.frame_limit,
            json!({"jsonrpc":"2.0","id":"occupied","result":{}}),
        )
        .unwrap();
        assert!(bus
            .dispatch_with_response(
                &beta.reader,
                "bus/subscribe",
                json!({"binding_id":binding(&bus),"topic":"bus.alpha.status"}),
                |result| queue_provider_host_response(&beta.reader, &id, Ok(result.unwrap())),
            )
            .is_err());
        assert!(beta.reader.closed.load(Ordering::Acquire));
        assert!(lock_std_mutex(&beta.reader.child_requests).is_empty());
        assert_eq!(publish(&bus, &alpha.reader).unwrap()["sequence"], 1);
        drop(beta.frames.try_recv().unwrap());
        assert!(beta.no_data());
        let state = lock_std_mutex(&bus.inner);
        let peer = &state.peers[&(beta.reader.instance_id.clone(), beta.reader.generation)];
        assert!(peer
            .subscriptions
            .values()
            .all(CancellationToken::is_cancelled));
        assert_eq!(peer.budget.messages.load(Ordering::Acquire), 0);
        assert_eq!(peer.budget.bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn stale_captured_binding_is_refused_and_new_incarnation_is_required() {
        let bus = ExtensionEventBus::default();
        let alpha = attached(&bus, "alpha", 8);
        declare(&bus, &alpha.reader);
        let captured = binding(&bus);
        assert!(publish(&bus, &alpha.reader).is_ok());
        // A replacement incarnation invalidates the captured scope even though
        // the process instance and generation are unchanged.
        bus.reset();
        assert_eq!(
            bus.dispatch(
                &alpha.reader,
                "bus/publish",
                json!({"binding_id":captured,"topic":"bus.alpha.status","payload":{"summary":"safe"}}),
            )
            .unwrap_err()
            .code,
            -32011
        );
        assert_eq!(
            bus.dispatch(
                &alpha.reader,
                "bus/declare",
                json!({"binding_id":captured,"topic":"bus.alpha.status","fields":[
                    {"name":"summary","kind":"string","required":true,"max_bytes":1024,"values":[]}
                ]}),
            )
            .unwrap_err()
            .code,
            -32011
        );
        // The fresh host binding works again after the reset.
        assert_ne!(binding(&bus), captured);
        declare(&bus, &alpha.reader);
        assert!(publish(&bus, &alpha.reader).is_ok());
    }

    #[test]
    fn unattached_processes_never_reach_bus_mutation() {
        let bus = ExtensionEventBus::default();
        let (alpha, _a) = peer("alpha", 8);
        assert_eq!(
            bus.dispatch(
                &alpha,
                "bus/declare",
                json!({"binding_id":binding(&bus),"topic":"bus.alpha.status","fields":[
                    {"name":"summary","kind":"string","required":true,"max_bytes":1024,"values":[]}
                ]}),
            )
            .unwrap_err()
            .code,
            -32011
        );
    }

    #[test]
    fn fanout_pressure_is_atomic_and_does_not_consume_sequence() {
        let bus = ExtensionEventBus::default();
        let alpha = attached(&bus, "alpha", 8);
        let mut beta = attached(&bus, "beta", 8);
        let mut gamma = attached(&bus, "gamma", 1);
        declare(&bus, &alpha.reader);
        subscribe(&bus, &beta.reader);
        subscribe(&bus, &gamma.reader);
        beta.drain_control();
        gamma.drain_control();
        assert_eq!(publish(&bus, &alpha.reader).unwrap()["sequence"], 1);
        drop(beta.expect_data());
        assert_eq!(publish(&bus, &alpha.reader).unwrap_err().code, -32012);
        assert!(beta.no_data(), "pressure cannot partially fan out");
        drop(gamma.expect_data());
        assert_eq!(publish(&bus, &alpha.reader).unwrap()["sequence"], 2);
        assert!(beta
            .expect_data()
            .bus_delivery
            .as_ref()
            .unwrap()
            .is_current());
    }

    #[test]
    fn delivery_is_generation_subscription_session_and_age_fenced() {
        let bus = ExtensionEventBus::default();
        let alpha = attached(&bus, "alpha", 8);
        let mut beta = attached(&bus, "beta", 8);
        declare(&bus, &alpha.reader);
        subscribe(&bus, &beta.reader);
        beta.drain_control();
        publish(&bus, &alpha.reader).unwrap();
        let frame = beta.expect_data();
        unsubscribe(&bus, &beta.reader);
        assert!(!frame.bus_delivery.as_ref().unwrap().is_current());
        subscribe(&bus, &beta.reader);
        beta.drain_control();
        publish(&bus, &alpha.reader).unwrap();
        let mut aged = beta.expect_data();
        aged.bus_delivery.as_mut().unwrap().queued = Instant::now() - Duration::from_secs(31);
        assert!(!aged.bus_delivery.as_ref().unwrap().is_current());
        publish(&bus, &alpha.reader).unwrap();
        let frame = beta.expect_data();
        // Removing the publisher's real instance/generation detaches it, so a
        // later request is refused rather than silently mutating a stale peer.
        bus.remove(&alpha.reader.instance_id, alpha.reader.generation);
        assert!(!frame.bus_delivery.as_ref().unwrap().is_current());
        assert_eq!(publish(&bus, &alpha.reader).unwrap_err().code, -32011);
        let alpha = attached(&bus, "alpha", 8);
        declare(&bus, &alpha.reader);
        subscribe(&bus, &beta.reader);
        beta.drain_control();
        publish(&bus, &alpha.reader).unwrap();
        let frame = beta.expect_data();
        bus.reset();
        assert!(!frame.bus_delivery.as_ref().unwrap().is_current());
    }

    #[test]
    fn reset_keeps_pending_credits_and_process_sequence() {
        let bus = ExtensionEventBus::default();
        let alpha = attached(&bus, "alpha", 128);
        let mut beta = attached(&bus, "beta", 128);
        declare(&bus, &alpha.reader);
        subscribe(&bus, &beta.reader);
        beta.drain_control();

        // Exactly the bounded backlog is admitted; one more publication is
        // refused without advancing the publisher's sequence.
        let mut held = Vec::new();
        for _ in 0..api_v03::MAX_BUS_QUEUE_MESSAGES {
            publish(&bus, &alpha.reader).unwrap();
            held.push(beta.expect_data());
        }
        assert_eq!(publish(&bus, &alpha.reader).unwrap_err().code, -32012);

        // An incarnation change cannot silently drop the binding notice for a
        // peer with no credit left: that peer fails closed and is retired
        // rather than continuing on an apparently current stale binding.
        bus.reset();
        assert!(beta.reader.closed.load(Ordering::Acquire));
        assert!(held
            .iter()
            .all(|frame| !frame.bus_delivery.as_ref().unwrap().is_current()));
        assert_eq!(
            bus.dispatch(
                &beta.reader,
                "bus/subscribe",
                json!({"binding_id":binding(&bus),"topic":"bus.alpha.status"}),
            )
            .unwrap_err()
            .code,
            -32011
        );

        // Credits belong to the process connection, not to one incarnation or
        // topic revision: they stay charged until the retained frames are
        // dropped, then are refunded exactly once.
        let key = (beta.reader.instance_id.clone(), beta.reader.generation);
        assert_eq!(
            lock_std_mutex(&bus.inner).peers[&key]
                .budget
                .messages
                .load(Ordering::Acquire),
            api_v03::MAX_BUS_QUEUE_MESSAGES
        );
        held.clear();
        assert_eq!(
            lock_std_mutex(&bus.inner).peers[&key]
                .budget
                .messages
                .load(Ordering::Acquire),
            0
        );

        // The retained publisher instance keeps counting across a reset; its
        // sequence never rewinds to 1 for the same process generation.
        let mut delta = attached(&bus, "delta", 128);
        declare(&bus, &alpha.reader);
        subscribe(&bus, &delta.reader);
        delta.drain_control();
        assert_eq!(
            publish(&bus, &alpha.reader).unwrap()["sequence"],
            api_v03::MAX_BUS_QUEUE_MESSAGES + 1
        );
        drop(delta.expect_data());

        // A publisher reload is a new generation of the same process identity.
        // Old frames stay fenced, and the replacement silently rewinding or
        // refunding another process's backlog is impossible.
        let (mut replacement_reader, _r) = peer("alpha", 128);
        replacement_reader.instance_id = alpha.reader.instance_id.clone();
        replacement_reader.generation = alpha.reader.generation + 1;
        let replacement = Arc::new(replacement_reader);
        publish(&bus, &alpha.reader).unwrap();
        let stale = delta.expect_data();
        bus.remove(&alpha.reader.instance_id, alpha.reader.generation);
        bus.attach(&replacement).unwrap();
        assert!(!stale.bus_delivery.as_ref().unwrap().is_current());
        assert_eq!(publish(&bus, &alpha.reader).unwrap_err().code, -32011);
        declare(&bus, &replacement);
        assert_eq!(publish(&bus, &replacement).unwrap()["sequence"], 1);
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_partial_writes_cancel_on_owner_subscription_reset_or_expiry() {
        use std::future::Future;
        for action in [
            "reset",
            "publisher_reload",
            "subscriber_reload",
            "unsubscribe",
            "expiry",
        ] {
            let bus = ExtensionEventBus::default();
            let alpha = attached(&bus, "alpha", 8);
            let mut beta = attached(&bus, "beta", 8);
            declare(&bus, &alpha.reader);
            subscribe(&bus, &beta.reader);
            beta.drain_control();
            publish(&bus, &alpha.reader).unwrap();
            let frame = beta.expect_data();
            let delivery = frame.bus_delivery.as_ref().unwrap();
            let (mut sink, mut source) = tokio::io::duplex(1);
            let mut write = Box::pin(delivery.guard_write(async {
                sink.write_all(&frame.line)
                    .await
                    .map_err(|error| error.to_string())
            }));
            std::future::poll_fn(|cx| {
                assert!(write.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            match action {
                "reset" => bus.reset(),
                "publisher_reload" => {
                    bus.remove(&alpha.reader.instance_id, alpha.reader.generation)
                }
                "subscriber_reload" => bus.remove(&beta.reader.instance_id, beta.reader.generation),
                "unsubscribe" => unsubscribe(&bus, &beta.reader),
                "expiry" => {
                    tokio::time::advance(Duration::from_millis(
                        api_v03::MAX_BUS_MESSAGE_AGE_MS as u64 + 1,
                    ))
                    .await
                }
                _ => unreachable!(),
            }
            assert!(
                tokio::time::timeout(Duration::from_secs(1), write)
                    .await
                    .unwrap()
                    .is_err(),
                "{action}"
            );
            drop(sink);
            let mut written = Vec::new();
            source.read_to_end(&mut written).await.unwrap();
            assert_eq!(
                written,
                &frame.line[..1],
                "no complete stale frame: {action}"
            );
        }
    }

    #[test]
    fn subscription_requires_delivery_negotiation_and_byte_pressure_is_atomic() {
        let bus = ExtensionEventBus::default();
        let alpha = attached(&bus, "alpha", 8);
        let mut beta = attached(&bus, "beta", 8);
        let mut gamma = attached(&bus, "gamma", 8);
        let binding = binding(&bus);
        let fields: Vec<_> = (0..7).map(|n| json!({"name":format!("value{n}"),"kind":"string","required":true,"max_bytes":1024,"values":[]})).collect();
        bus.dispatch(
            &alpha.reader,
            "bus/declare",
            json!({"binding_id":binding,"topic":"bus.alpha.status","fields":fields}),
        )
        .unwrap();
        write_std_lock(&gamma.reader.api_v03_contract)
            .as_mut()
            .unwrap()
            .methods
            .remove("bus/event");
        assert_eq!(
            bus.dispatch(
                &gamma.reader,
                "bus/subscribe",
                json!({"binding_id":binding,"topic":"bus.alpha.status"})
            )
            .unwrap_err()
            .code,
            -32011
        );
        write_std_lock(&gamma.reader.api_v03_contract)
            .as_mut()
            .unwrap()
            .methods
            .insert("bus/event".into());
        subscribe(&bus, &beta.reader);
        subscribe(&bus, &gamma.reader);
        beta.drain_control();
        gamma.drain_control();
        let payload: serde_json::Map<_, _> = (0..7)
            .map(|n| (format!("value{n}"), json!("x".repeat(1024))))
            .collect();
        let request = json!({"binding_id":binding,"topic":"bus.alpha.status","payload":payload});
        let mut held = Vec::new();
        loop {
            match bus.dispatch(&alpha.reader, "bus/publish", request.clone()) {
                Ok(_) => {
                    drop(beta.expect_data());
                    held.push(gamma.expect_data());
                    assert!(
                        held.len() < api_v03::MAX_BUS_QUEUE_MESSAGES,
                        "byte bound must win before message bound"
                    );
                }
                Err(error) => {
                    assert_eq!(error.code, -32012);
                    break;
                }
            }
        }
        assert!(!held.is_empty());
        assert!(beta.no_data(), "byte pressure cannot partially fan out");
        assert!(gamma.no_data());
        let next = held.len() + 1;
        held.clear();
        assert_eq!(
            bus.dispatch(&alpha.reader, "bus/publish", request).unwrap()["sequence"],
            next
        );
        drop(beta.expect_data());
        drop(gamma.expect_data());
    }

    #[test]
    fn bounds_include_dequeued_but_unwritten_frames_and_reject_unsafe_fields() {
        let bus = ExtensionEventBus::default();
        let alpha = attached(&bus, "alpha", 128);
        let mut beta = attached(&bus, "beta", 128);
        let binding = binding(&bus);
        declare(&bus, &alpha.reader);
        subscribe(&bus, &beta.reader);
        beta.drain_control();
        let mut held = Vec::new();
        for _ in 0..api_v03::MAX_BUS_QUEUE_MESSAGES {
            publish(&bus, &alpha.reader).unwrap();
            held.push(beta.expect_data());
        }
        assert_eq!(publish(&bus, &alpha.reader).unwrap_err().code, -32012);
        held.clear();
        assert!(publish(&bus, &alpha.reader).is_ok());
        for field in ["api_key", "trust", "path", "resource_handle", "session"] {
            assert!(bus
                .dispatch(
                    &alpha.reader,
                    "bus/declare",
                    json!({"binding_id":binding,"topic":"bus.alpha.bad","fields":[
                        {"name":field,"kind":"string","required":true,"max_bytes":1024,"values":[]}
                    ]})
                )
                .is_err());
        }
        for text in [
            "Bearer hidden",
            "user@example.com",
            "sk-abcdefgh",
            "/private/file",
            "~/.ssh/id",
            "+1 (555) 010-9999",
            "bad\nline",
        ] {
            assert!(bus
                .dispatch(
                    &alpha.reader,
                    "bus/publish",
                    json!({"binding_id":binding,"topic":"bus.alpha.status","payload":{"summary":text}})
                )
                .is_err());
        }
        assert_eq!(
            bus.dispatch(
                &beta.reader,
                "bus/publish",
                json!({"binding_id":binding,"topic":"bus.alpha.status","payload":{"summary":"safe"}})
            )
            .unwrap_err()
            .code,
            -32011
        );
    }
}

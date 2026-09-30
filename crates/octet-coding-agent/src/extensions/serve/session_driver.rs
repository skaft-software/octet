//! The session driver the graphical host runs each session through.

use super::*;

pub(super) struct OctetSessionDriver {
    pub(super) seed: SessionSeed,
    pub(super) commands: Option<mpsc::Sender<WorkerMessage>>,
    pub(super) events: mpsc::Receiver<TimestampedEvent>,
    pub(super) buffered_events: VecDeque<TimestampedEvent>,
    pub(super) worker: Option<tokio::task::JoinHandle<()>>,
    pub(super) inspect_only: bool,
    pub(super) inspection: Option<DelegatedInspection>,
}

impl OctetSessionDriver {
    pub(super) fn spawn(seed: SessionSeed, plan: WorkerPlan, known_entries: usize) -> Self {
        let (commands, command_receiver) = mpsc::channel(DRIVER_MAILBOX_CAPACITY);
        let (event_sender, events) = mpsc::channel(DRIVER_EVENT_CAPACITY);
        let worker = tokio::spawn(run_worker(
            plan,
            command_receiver,
            event_sender,
            known_entries,
        ));
        Self {
            seed,
            commands: Some(commands),
            events,
            buffered_events: VecDeque::new(),
            worker: Some(worker),
            inspect_only: false,
            inspection: None,
        }
    }

    pub(super) fn inspect(
        seed: SessionSeed,
        refresh: DelegatedInspectionRefresh,
        fingerprint: DelegatedSessionFingerprint,
    ) -> Self {
        let (sender, events) = mpsc::channel(1);
        drop(sender);
        let projection = seed.clone();
        Self {
            seed,
            commands: None,
            events,
            buffered_events: VecDeque::new(),
            worker: None,
            inspect_only: true,
            inspection: Some(DelegatedInspection {
                refresh,
                fingerprint,
                projection,
            }),
        }
    }
}

#[async_trait]
impl SessionDriver for OctetSessionDriver {
    fn seed(&self) -> SessionSeed {
        self.seed.clone()
    }

    async fn dispatch(
        &mut self,
        command: SessionCommand,
    ) -> Result<DriverCommandOutcome, ServiceError> {
        if self.inspect_only {
            return Err(ServiceError::Unauthorized);
        }
        if let SessionCommand::SetAuthority { authority } = command {
            // Reject unsupported narrowing before the worker mailbox, even
            // during a run. The seed describes immutable host authority; a
            // repeated selection needs no rebuild, settings event, or effect.
            return if authority == self.seed.snapshot.authority {
                Ok(DriverCommandOutcome::default())
            } else {
                Err(ServiceError::Unauthorized)
            };
        }
        let (response, receiver) = oneshot::channel();
        self.commands
            .as_ref()
            .ok_or(ServiceError::OwnerLost)?
            .send(WorkerMessage::Command(WorkerCommand { command, response }))
            .await
            .map_err(|_| ServiceError::Unavailable)?;
        receiver.await.map_err(|_| ServiceError::Unavailable)?
    }

    async fn command_discovery(&mut self) -> Result<CommandDiscovery, ServiceError> {
        if self.inspect_only {
            return Ok(CommandDiscovery {
                protocol: PROTOCOL_VERSION,
                commands: Vec::new(),
                skills: Vec::new(),
            });
        }
        let (response, mut receiver) = oneshot::channel();
        self.commands
            .as_ref()
            .ok_or(ServiceError::OwnerLost)?
            .send(WorkerMessage::CommandDiscovery { response })
            .await
            .map_err(|_| ServiceError::Unavailable)?;

        // The actor serializes this call with `next_event`. Keep receiving into
        // a private FIFO while the worker processes discovery so a busy stream
        // cannot fill the worker's event channel and block its command select.
        // If the FIFO reaches its bound, stop draining briefly so the worker can
        // answer; otherwise fail the discovery request and let the actor resume
        // normal event reduction without dropping stream events.
        let mut events_open = true;
        loop {
            if events_open && self.buffered_events.len() >= MAX_BUFFERED_DISCOVERY_EVENTS {
                let result = tokio::time::timeout(DISCOVERY_BACKPRESSURE_TIMEOUT, &mut receiver)
                    .await
                    .map_err(|_| ServiceError::Unavailable)?
                    .map_err(|_| ServiceError::Unavailable)?;
                return result;
            }
            tokio::select! {
                result = &mut receiver => return result.map_err(|_| ServiceError::Unavailable)?,
                event = self.events.recv(), if events_open => match event {
                    Some(event) => self.buffered_events.push_back(event),
                    None => events_open = false,
                },
            }
        }
    }

    async fn next_event(&mut self) -> Option<TimestampedEvent> {
        if self.inspect_only {
            loop {
                if let Some(event) = self.buffered_events.pop_front() {
                    return Some(event);
                }
                let inspection = self.inspection.as_ref()?;
                let refresh = inspection.refresh.clone();
                let fingerprint = inspection.fingerprint;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let loaded = tokio::task::spawn_blocking(move || refresh.load(fingerprint)).await;
                match loaded {
                    Ok(Ok(None)) => {}
                    Ok(Ok(Some((next_fingerprint, next)))) => {
                        let inspection = self.inspection.as_mut()?;
                        let Some(events) =
                            delegated_inspection_events(&inspection.projection, &next)
                        else {
                            self.inspection = None;
                            return Some(TimestampedEvent::new(
                                now_ms(),
                                EventPayload::SessionStateChanged {
                                    state: SessionLiveState::Offline,
                                    active_run_id: None,
                                },
                            ));
                        };
                        inspection.fingerprint = next_fingerprint;
                        inspection.projection = next;
                        self.seed = inspection.projection.clone();
                        self.buffered_events = events;
                    }
                    Ok(Err(_)) | Err(_) => {
                        self.inspection = None;
                        return Some(TimestampedEvent::new(
                            now_ms(),
                            EventPayload::SessionStateChanged {
                                state: SessionLiveState::Offline,
                                active_run_id: None,
                            },
                        ));
                    }
                }
            }
        }
        match self.buffered_events.pop_front() {
            Some(event) => Some(event),
            None => self.events.recv().await,
        }
    }

    async fn shutdown(&mut self) {
        self.commands.take();
        self.events.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.await;
        }
    }
}

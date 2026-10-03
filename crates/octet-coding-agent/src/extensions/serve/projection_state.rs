//! Per-run projection state shared by the run driver and the projections.

use super::*;

pub(super) enum PrivateResponse {
    Approval(Box<dyn FnOnce(bool) + Send + Sync>),
    Input(Box<dyn FnOnce(Option<Vec<u8>>) + Send + Sync>),
}

pub(super) struct PrivateRequest {
    pub(super) kind: RequestKind,
    pub(super) response: PrivateResponse,
}

#[derive(Clone)]
pub(super) struct ProjectedToolCall {
    pub(super) name: String,
    pub(super) arguments: serde_json::Value,
    pub(super) activity: ToolActivity,
    pub(super) result: Option<ToolResultSummary>,
    pub(super) turn_id: TurnId,
}

#[derive(Clone, Default)]
pub(super) struct ProjectedToolProgress {
    pub(super) observed_output_bytes: u64,
    pub(super) dropped_output_bytes: u64,
}

pub(super) struct CompletedToolEvidence {
    pub(super) tool_call_id: String,
    pub(super) tool_item_id: ItemId,
    pub(super) turn_id: TurnId,
    pub(super) tool: ProjectedToolCall,
    pub(super) output: ToolOutput,
}

pub(super) struct PendingUserItem {
    pub(super) id: ItemId,
    pub(super) delivery: UserMessageDelivery,
    pub(super) turn_id: TurnId,
    pub(super) documents: Vec<DocumentReference>,
    pub(super) project_files: Vec<TrustedFileEntry>,
    pub(super) document_context_tokens: u64,
    pub(super) project_file_context_tokens: u64,
    pub(super) context_attributed: bool,
    pub(super) branch_provenance: Option<ConversationBranchProvenance>,
}

pub(super) struct RunContextProjection {
    pub(super) usage_uncertain: bool,
    pub(super) last_agent_snapshot: Option<AgentContextSnapshot>,
    pub(super) last_published: Option<ContextUsage>,
    pub(super) current_totals: Option<ContextTotals>,
    pub(super) context_updated_at_ms: u64,
    pub(super) active_compaction: Option<(u64, ActiveCompaction)>,
    pub(super) last_compaction: Option<(u64, CompletedCompaction)>,
    pub(super) project_instruction_tokens: u64,
    pub(super) document_context_tokens: u64,
    pub(super) project_file_context_tokens: u64,
}

impl RunContextProjection {
    pub(super) fn new(
        project_instruction_tokens: u64,
        document_context_tokens: u64,
        project_file_context_tokens: u64,
    ) -> Self {
        Self {
            usage_uncertain: false,
            last_agent_snapshot: None,
            last_published: None,
            current_totals: None,
            context_updated_at_ms: 0,
            active_compaction: None,
            last_compaction: None,
            project_instruction_tokens,
            document_context_tokens,
            project_file_context_tokens,
        }
    }

    pub(super) fn attribute_sources(&mut self, document_tokens: u64, project_file_tokens: u64) {
        self.document_context_tokens = self.document_context_tokens.saturating_add(document_tokens);
        self.project_file_context_tokens = self
            .project_file_context_tokens
            .saturating_add(project_file_tokens);
    }

    pub(super) fn clear_auxiliary_sources(&mut self) {
        self.document_context_tokens = 0;
        self.project_file_context_tokens = 0;
    }
}

pub(super) struct ResolvedPromptInput {
    pub(super) display_text: String,
    pub(super) model_text: String,
    pub(super) attachments: Vec<AttachmentRef>,
    pub(super) documents: Vec<DocumentReference>,
    pub(super) project_files: Vec<TrustedFileEntry>,
    pub(super) document_context_tokens: u64,
    pub(super) project_file_context_tokens: u64,
}

pub(super) enum RunPromptInput {
    New(PromptInput),
    Replay(ResolvedPromptInput),
}

pub(super) enum RunDriveOutcome {
    Admitted {
        goal: Option<octet_agent::GoalDecision>,
    },
    Rejected {
        admission: Option<oneshot::Sender<Result<DriverCommandOutcome, ServiceError>>>,
        error: ServiceError,
    },
}

pub(super) struct ProjectionState {
    pub(super) usage_uncertain: bool,
    pub(super) last_context: Option<ContextUsage>,
    pub(super) known_entries: usize,
    pub(super) run_counter: u64,
    pub(super) user_item_counter: u64,
    pub(super) request_counter: u64,
    pub(super) turn_counter: u64,
    pub(super) provider_attempt: u32,
    pub(super) assistant_item: Option<ItemId>,
    pub(super) reasoning_item: Option<ItemId>,
    pub(super) completed_assistant_items: VecDeque<Option<(ItemId, TurnId)>>,
    pub(super) completed_reasoning_items: VecDeque<Option<(ItemId, TurnId)>>,
    pub(super) tool_items: HashMap<String, ItemId>,
    pub(super) tool_calls: HashMap<String, ProjectedToolCall>,
    pub(super) pending_tool_evidence: VecDeque<CompletedToolEvidence>,
    pub(super) tool_progress: HashMap<String, ProjectedToolProgress>,
    pub(super) test_results: Vec<StructuredTestResults>,
    pub(super) item_turns: HashMap<ItemId, TurnId>,
    pub(super) run_started_at_ms: u64,
    pub(super) private_requests: HashMap<RequestId, PrivateRequest>,
    pub(super) pending_attachments: VecDeque<Vec<AttachmentRef>>,
    pub(super) pending_user_items: VecDeque<PendingUserItem>,
    pub(super) extension_presentations: Vec<ExtensionPresentation>,
}

impl ProjectionState {
    pub(super) fn new(known_entries: usize) -> Self {
        Self {
            usage_uncertain: false,
            last_context: None,
            known_entries,
            run_counter: 0,
            user_item_counter: 0,
            request_counter: 0,
            turn_counter: 1,
            provider_attempt: 1,
            assistant_item: None,
            reasoning_item: None,
            completed_assistant_items: VecDeque::new(),
            completed_reasoning_items: VecDeque::new(),
            tool_items: HashMap::new(),
            tool_calls: HashMap::new(),
            pending_tool_evidence: VecDeque::new(),
            tool_progress: HashMap::new(),
            test_results: Vec::new(),
            item_turns: HashMap::new(),
            run_started_at_ms: now_ms(),
            private_requests: HashMap::new(),
            pending_attachments: VecDeque::new(),
            pending_user_items: VecDeque::new(),
            extension_presentations: Vec::new(),
        }
    }

    pub(super) fn next_run_id(&mut self, generation: u64) -> Result<RunId, ServiceError> {
        self.run_counter = self
            .run_counter
            .checked_add(1)
            .ok_or(ServiceError::Internal)?;
        RunId::new(format!("run-{generation}-{}", self.run_counter))
            .map_err(|_| ServiceError::Internal)
    }

    pub(super) fn begin_run(&mut self) {
        self.user_item_counter = 0;
        self.turn_counter = 1;
        self.provider_attempt = 1;
        self.assistant_item = None;
        self.reasoning_item = None;
        self.completed_assistant_items.clear();
        self.completed_reasoning_items.clear();
        self.tool_items.clear();
        self.tool_calls.clear();
        self.tool_progress.clear();
        self.test_results.clear();
        self.item_turns.clear();
        self.run_started_at_ms = now_ms();
        self.private_requests.clear();
        self.pending_attachments.clear();
        self.pending_user_items.clear();
    }

    pub(super) fn next_user_item_id(&mut self, run_id: &RunId) -> Result<ItemId, ServiceError> {
        self.user_item_counter = self
            .user_item_counter
            .checked_add(1)
            .ok_or(ServiceError::Internal)?;
        self.provisional_id(run_id, "user", self.user_item_counter)
    }

    pub(super) fn turn_id(&self, run_id: &RunId) -> Result<TurnId, ServiceError> {
        TurnId::new(format!("turn-{}-{}", run_id.as_str(), self.turn_counter))
            .map_err(|_| ServiceError::Internal)
    }

    pub(super) fn provisional_id(
        &self,
        run_id: &RunId,
        kind: &str,
        suffix: u64,
    ) -> Result<ItemId, ServiceError> {
        ItemId::new(format!(
            "item-{}-{kind}-{}-{suffix}",
            run_id.as_str(),
            self.turn_counter
        ))
        .map_err(|_| ServiceError::Internal)
    }

    pub(super) fn finish_turn(&mut self) {
        let turn_id = self
            .assistant_item
            .as_ref()
            .or(self.reasoning_item.as_ref())
            .and_then(|item_id| self.item_turns.get(item_id))
            .cloned();
        self.completed_assistant_items
            .push_back(self.assistant_item.take().zip(turn_id.clone()));
        self.completed_reasoning_items
            .push_back(self.reasoning_item.take().zip(turn_id));
        self.turn_counter = self.turn_counter.saturating_add(1);
        self.provider_attempt = 1;
    }
}

pub(super) fn collect_extension_presentations(
    extensions: &mut crate::extensions::ExecutableExtensions,
) -> Vec<ExtensionPresentation> {
    let _ = extensions.drain_events();
    extensions
        .presentation_views()
        .into_iter()
        .map(|view| ExtensionPresentation {
            extension: view.extension,
            generation: view.generation,
            extension_instance_id: view.extension_instance_id,
            resource_owner: view.resource_owner,
            snapshot: view.snapshot,
        })
        .collect()
}

pub(super) async fn publish_extension_presentations(
    extensions: &mut crate::extensions::ExecutableExtensions,
    projection: &mut ProjectionState,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let presentations = collect_extension_presentations(extensions);
    if presentations == projection.extension_presentations {
        return Ok(());
    }
    projection.extension_presentations = presentations.clone();
    events
        .send(event(EventPayload::ExtensionPresentationsChanged {
            presentations,
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

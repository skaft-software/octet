import Foundation

public struct HostDescriptor: Codable, Equatable, Sendable {
    public let id: HostId; public let name: String
    private enum CodingKeys: String, CodingKey { case id, name }
    public init(id: HostId, name: String) { self.id = id; self.name = name }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["id", "name"], path: "host"); id = try c.decode(HostId.self, forKey: .id); name = try c.decode(String.self, forKey: .name) }
}

public struct AttachmentPolicy: Codable, Equatable, Sendable {
    public let acceptedMediaTypes: [String]; public let maxCount: UInt32; public let maxFileBytes: UInt64; public let maxTotalBytes: UInt64
    private enum CodingKeys: String, CodingKey { case acceptedMediaTypes, maxCount, maxFileBytes, maxTotalBytes }
    public init(acceptedMediaTypes: [String], maxCount: UInt32, maxFileBytes: UInt64, maxTotalBytes: UInt64) { self.acceptedMediaTypes = acceptedMediaTypes; self.maxCount = maxCount; self.maxFileBytes = maxFileBytes; self.maxTotalBytes = maxTotalBytes }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["acceptedMediaTypes", "maxCount", "maxFileBytes", "maxTotalBytes"], path: "attachmentPolicy"); acceptedMediaTypes = try c.decode([String].self, forKey: .acceptedMediaTypes); maxCount = try c.decode(UInt32.self, forKey: .maxCount); maxFileBytes = try c.decode(UInt64.self, forKey: .maxFileBytes); maxTotalBytes = try c.decode(UInt64.self, forKey: .maxTotalBytes) }
}

public struct HostCapabilities: Codable, Equatable, Sendable {
    public let concurrentSessions: Bool
    public let opaqueResources: Bool
    public let attachments: Bool
    public let attachmentPolicy: AttachmentPolicy?
    public let documents: Bool
    public let trustedProjectFiles: Bool
    public let projectFileBrowser: Bool
    public let projectFileWrite: Bool
    public let transcriptSearch: Bool
    public let previews: Bool
    public let connectedDevices: Bool
    public let sessionMetadata: Bool
    public let sessionBranches: Bool
    public let conversationBranching: Bool
    public let sessionTrash: Bool
    public let sessionExport: Bool
    public let lanClients: Bool
    public let terminal: Bool
    public let childAgents: Bool

    private enum CodingKeys: String, CodingKey { case concurrentSessions, opaqueResources, attachments, attachmentPolicy, documents, trustedProjectFiles, projectFileBrowser, projectFileWrite, transcriptSearch, previews, connectedDevices, sessionMetadata, sessionBranches, conversationBranching, sessionTrash, sessionExport, lanClients, terminal, childAgents }
    public init(concurrentSessions: Bool, opaqueResources: Bool, attachments: Bool, attachmentPolicy: AttachmentPolicy? = nil, documents: Bool = false, trustedProjectFiles: Bool = false, projectFileBrowser: Bool = false, projectFileWrite: Bool = false, transcriptSearch: Bool = false, previews: Bool, connectedDevices: Bool, sessionMetadata: Bool, sessionBranches: Bool, conversationBranching: Bool = false, sessionTrash: Bool = false, sessionExport: Bool, lanClients: Bool, terminal: Bool, childAgents: Bool) { self.concurrentSessions = concurrentSessions; self.opaqueResources = opaqueResources; self.attachments = attachments; self.attachmentPolicy = attachmentPolicy; self.documents = documents; self.trustedProjectFiles = trustedProjectFiles; self.projectFileBrowser = projectFileBrowser; self.projectFileWrite = projectFileWrite; self.transcriptSearch = transcriptSearch; self.previews = previews; self.connectedDevices = connectedDevices; self.sessionMetadata = sessionMetadata; self.sessionBranches = sessionBranches; self.conversationBranching = conversationBranching; self.sessionTrash = sessionTrash; self.sessionExport = sessionExport; self.lanClients = lanClients; self.terminal = terminal; self.childAgents = childAgents }
    public init(from decoder: Decoder) throws {
        let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["concurrentSessions", "opaqueResources", "attachments", "attachmentPolicy", "documents", "trustedProjectFiles", "projectFileBrowser", "projectFileWrite", "transcriptSearch", "previews", "connectedDevices", "sessionMetadata", "sessionBranches", "conversationBranching", "sessionTrash", "sessionExport", "lanClients", "terminal", "childAgents"], path: "capabilities")
        concurrentSessions = try c.decode(Bool.self, forKey: .concurrentSessions); opaqueResources = try c.decode(Bool.self, forKey: .opaqueResources); attachments = try c.decode(Bool.self, forKey: .attachments); attachmentPolicy = try c.decodeIfPresent(AttachmentPolicy.self, forKey: .attachmentPolicy)
        documents = try decodeDefault(c, Bool.self, forKey: .documents, default: false); trustedProjectFiles = try decodeDefault(c, Bool.self, forKey: .trustedProjectFiles, default: false); projectFileBrowser = try decodeDefault(c, Bool.self, forKey: .projectFileBrowser, default: false); projectFileWrite = try decodeDefault(c, Bool.self, forKey: .projectFileWrite, default: false); transcriptSearch = try decodeDefault(c, Bool.self, forKey: .transcriptSearch, default: false)
        previews = try c.decode(Bool.self, forKey: .previews); connectedDevices = try c.decode(Bool.self, forKey: .connectedDevices); sessionMetadata = try c.decode(Bool.self, forKey: .sessionMetadata); sessionBranches = try c.decode(Bool.self, forKey: .sessionBranches)
        conversationBranching = try decodeDefault(c, Bool.self, forKey: .conversationBranching, default: false); sessionTrash = try decodeDefault(c, Bool.self, forKey: .sessionTrash, default: false); sessionExport = try c.decode(Bool.self, forKey: .sessionExport); lanClients = try c.decode(Bool.self, forKey: .lanClients); terminal = try c.decode(Bool.self, forKey: .terminal); childAgents = try c.decode(Bool.self, forKey: .childAgents)
    }
}

public struct ProjectSummary: Codable, Equatable, Sendable {
    public let id: ProjectId; public let name: String; public let trusted: Bool; public let archived: Bool; public let available: Bool; public let isDefault: Bool; public let sessionCount: UInt32; public let liveSessionCount: UInt32
    private enum CodingKeys: String, CodingKey { case id, name, trusted, archived, available, isDefault, sessionCount, liveSessionCount }
    public init(id: ProjectId, name: String, trusted: Bool, archived: Bool, available: Bool, isDefault: Bool, sessionCount: UInt32, liveSessionCount: UInt32) { self.id = id; self.name = name; self.trusted = trusted; self.archived = archived; self.available = available; self.isDefault = isDefault; self.sessionCount = sessionCount; self.liveSessionCount = liveSessionCount }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["id", "name", "trusted", "archived", "available", "isDefault", "sessionCount", "liveSessionCount"], path: "project"); id = try c.decode(ProjectId.self, forKey: .id); name = try c.decode(String.self, forKey: .name); trusted = try c.decode(Bool.self, forKey: .trusted); archived = try c.decode(Bool.self, forKey: .archived); available = try c.decode(Bool.self, forKey: .available); isDefault = try c.decode(Bool.self, forKey: .isDefault); sessionCount = try c.decode(UInt32.self, forKey: .sessionCount); liveSessionCount = try c.decode(UInt32.self, forKey: .liveSessionCount) }
}

public struct ModelSelection: Codable, Equatable, Sendable {
    public let provider: String; public let model: String; public let reasoning: String
    private enum CodingKeys: String, CodingKey { case provider, model, reasoning }
    public init(provider: String, model: String, reasoning: String) { self.provider = provider; self.model = model; self.reasoning = reasoning }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["provider", "model", "reasoning"], path: "model"); provider = try c.decode(String.self, forKey: .provider); model = try c.decode(String.self, forKey: .model); reasoning = try c.decode(String.self, forKey: .reasoning) }
}

public struct ModelInputPricingTier: Codable, Equatable, Sendable {
    public let minInputTokens: UInt64; public let microdollarsPerMillionTokens: UInt64
    private enum CodingKeys: String, CodingKey { case minInputTokens, microdollarsPerMillionTokens }
    public init(minInputTokens: UInt64, microdollarsPerMillionTokens: UInt64) { self.minInputTokens = minInputTokens; self.microdollarsPerMillionTokens = microdollarsPerMillionTokens }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["minInputTokens", "microdollarsPerMillionTokens"], path: "pricingTier"); minInputTokens = try c.decode(UInt64.self, forKey: .minInputTokens); microdollarsPerMillionTokens = try c.decode(UInt64.self, forKey: .microdollarsPerMillionTokens) }
}

public struct ModelInputPricing: Codable, Equatable, Sendable {
    public let baseMicrodollarsPerMillionTokens: UInt64; public let tiers: [ModelInputPricingTier]
    private enum CodingKeys: String, CodingKey { case baseMicrodollarsPerMillionTokens, tiers }
    public init(baseMicrodollarsPerMillionTokens: UInt64, tiers: [ModelInputPricingTier]) { self.baseMicrodollarsPerMillionTokens = baseMicrodollarsPerMillionTokens; self.tiers = tiers }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["baseMicrodollarsPerMillionTokens", "tiers"], path: "inputPricing"); baseMicrodollarsPerMillionTokens = try c.decode(UInt64.self, forKey: .baseMicrodollarsPerMillionTokens); tiers = try c.decode([ModelInputPricingTier].self, forKey: .tiers) }
}

public struct ModelSummary: Codable, Equatable, Sendable {
    public let id: String; public let name: String; public let provider: String; public let local: Bool; public let available: Bool; public let reasoning: [String]; public let defaultReasoning: String?; public let inputPricing: ModelInputPricing?; public let inputModalities: [InputModality]
    private enum CodingKeys: String, CodingKey { case id, name, provider, local, available, reasoning, defaultReasoning, inputPricing, inputModalities }
    public init(id: String, name: String, provider: String, local: Bool, available: Bool, reasoning: [String], defaultReasoning: String? = nil, inputPricing: ModelInputPricing? = nil, inputModalities: [InputModality]) { self.id = id; self.name = name; self.provider = provider; self.local = local; self.available = available; self.reasoning = reasoning; self.defaultReasoning = defaultReasoning; self.inputPricing = inputPricing; self.inputModalities = inputModalities }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["id", "name", "provider", "local", "available", "reasoning", "defaultReasoning", "inputPricing", "inputModalities"], path: "modelSummary"); id = try c.decode(String.self, forKey: .id); name = try c.decode(String.self, forKey: .name); provider = try c.decode(String.self, forKey: .provider); local = try c.decode(Bool.self, forKey: .local); available = try c.decode(Bool.self, forKey: .available); reasoning = try c.decode([String].self, forKey: .reasoning); defaultReasoning = try c.decodeIfPresent(String.self, forKey: .defaultReasoning); inputPricing = try c.decodeIfPresent(ModelInputPricing.self, forKey: .inputPricing); inputModalities = try c.decode([InputModality].self, forKey: .inputModalities) }
}

public struct PullRequestSummary: Codable, Equatable, Sendable {
    public let state: PullRequestState
    private enum CodingKeys: String, CodingKey { case state }
    public init(state: PullRequestState) { self.state = state }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["state"], path: "pullRequest"); state = try c.decode(PullRequestState.self, forKey: .state) }
}

public struct SessionRetention: Codable, Equatable, Sendable {
    public let trashedAtMs: UInt64; public let purgeAfterMs: UInt64; public let permanentDeleteRequiresConfirmation: Bool
    private enum CodingKeys: String, CodingKey { case trashedAtMs, purgeAfterMs, permanentDeleteRequiresConfirmation }
    public init(trashedAtMs: UInt64, purgeAfterMs: UInt64, permanentDeleteRequiresConfirmation: Bool) { self.trashedAtMs = trashedAtMs; self.purgeAfterMs = purgeAfterMs; self.permanentDeleteRequiresConfirmation = permanentDeleteRequiresConfirmation }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["trashedAtMs", "purgeAfterMs", "permanentDeleteRequiresConfirmation"], path: "retention"); trashedAtMs = try c.decode(UInt64.self, forKey: .trashedAtMs); purgeAfterMs = try c.decode(UInt64.self, forKey: .purgeAfterMs); permanentDeleteRequiresConfirmation = try c.decode(Bool.self, forKey: .permanentDeleteRequiresConfirmation) }
}

public struct ConversationBranchProvenance: Codable, Equatable, Sendable {
    public let operation: ConversationBranchOperation; public let sourceSessionId: SessionId; public let sourceEntryId: DurableEntryId; public let originatingUserEntryId: DurableEntryId?; public let modelOverride: ModelSelection?; public let externalEffectsPreserved: Bool; public let warning: String
    private enum CodingKeys: String, CodingKey { case operation, sourceSessionId, sourceEntryId, originatingUserEntryId, modelOverride, externalEffectsPreserved, warning }
    public init(operation: ConversationBranchOperation, sourceSessionId: SessionId, sourceEntryId: DurableEntryId, originatingUserEntryId: DurableEntryId? = nil, modelOverride: ModelSelection? = nil, externalEffectsPreserved: Bool, warning: String) { self.operation = operation; self.sourceSessionId = sourceSessionId; self.sourceEntryId = sourceEntryId; self.originatingUserEntryId = originatingUserEntryId; self.modelOverride = modelOverride; self.externalEffectsPreserved = externalEffectsPreserved; self.warning = warning }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["operation", "sourceSessionId", "sourceEntryId", "originatingUserEntryId", "modelOverride", "externalEffectsPreserved", "warning"], path: "branchProvenance"); operation = try c.decode(ConversationBranchOperation.self, forKey: .operation); sourceSessionId = try c.decode(SessionId.self, forKey: .sourceSessionId); sourceEntryId = try c.decode(DurableEntryId.self, forKey: .sourceEntryId); originatingUserEntryId = try c.decodeIfPresent(DurableEntryId.self, forKey: .originatingUserEntryId); modelOverride = try c.decodeIfPresent(ModelSelection.self, forKey: .modelOverride); externalEffectsPreserved = try c.decode(Bool.self, forKey: .externalEffectsPreserved); warning = try c.decode(String.self, forKey: .warning) }
}

public struct SessionBranchEntry: Codable, Equatable, Sendable {
    public let entryId: DurableEntryId; public let parentEntryId: DurableEntryId?; public let kind: SessionBranchEntryKind; public let checkoutable: Bool; public let label: String
    private enum CodingKeys: String, CodingKey { case entryId, parentEntryId, kind, checkoutable, label }
    public init(entryId: DurableEntryId, parentEntryId: DurableEntryId?, kind: SessionBranchEntryKind, checkoutable: Bool, label: String) { self.entryId = entryId; self.parentEntryId = parentEntryId; self.kind = kind; self.checkoutable = checkoutable; self.label = label }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["entryId", "parentEntryId", "kind", "checkoutable", "label"], path: "branchEntry"); entryId = try c.decode(DurableEntryId.self, forKey: .entryId); parentEntryId = try c.decodeIfPresent(DurableEntryId.self, forKey: .parentEntryId); kind = try c.decode(SessionBranchEntryKind.self, forKey: .kind); checkoutable = try c.decode(Bool.self, forKey: .checkoutable); label = try c.decode(String.self, forKey: .label) }
}

public struct SessionBranchGraph: Codable, Equatable, Sendable {
    public let head: DurableEntryId?; public let entries: [SessionBranchEntry]; public let truncated: Bool
    private enum CodingKeys: String, CodingKey { case head, entries, truncated }
    public init(head: DurableEntryId?, entries: [SessionBranchEntry], truncated: Bool = false) { self.head = head; self.entries = entries; self.truncated = truncated }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["head", "entries", "truncated"], path: "branches"); head = try c.decodeIfPresent(DurableEntryId.self, forKey: .head); entries = try c.decode([SessionBranchEntry].self, forKey: .entries); truncated = try decodeDefault(c, Bool.self, forKey: .truncated, default: false) }
}

public struct SessionSummary: Codable, Equatable, Sendable {
    public let id: SessionId; public let projectId: ProjectId?; public let title: String; public let tags: [String]; public let createdAtMs: UInt64; public let modifiedAtMs: UInt64; public let pinned: Bool; public let archived: Bool; public let lifecycle: SessionCatalogState; public let retention: SessionRetention?; public let forkedFrom: ConversationBranchProvenance?; public let provisional: Bool; public let liveState: SessionLiveState; public let attention: AttentionState; public let pullRequest: PullRequestSummary?; public let owner: ActorOwnerState; public let model: ModelSelection
    private enum CodingKeys: String, CodingKey { case id, projectId, title, tags, createdAtMs, modifiedAtMs, pinned, archived, lifecycle, retention, forkedFrom, provisional, liveState, attention, pullRequest, owner, model }
    public init(id: SessionId, projectId: ProjectId?, title: String, tags: [String] = [], createdAtMs: UInt64, modifiedAtMs: UInt64, pinned: Bool, archived: Bool, lifecycle: SessionCatalogState = .active, retention: SessionRetention? = nil, forkedFrom: ConversationBranchProvenance? = nil, provisional: Bool, liveState: SessionLiveState, attention: AttentionState, pullRequest: PullRequestSummary? = nil, owner: ActorOwnerState, model: ModelSelection) { self.id=id; self.projectId=projectId; self.title=title; self.tags=tags; self.createdAtMs=createdAtMs; self.modifiedAtMs=modifiedAtMs; self.pinned=pinned; self.archived=archived; self.lifecycle=lifecycle; self.retention=retention; self.forkedFrom=forkedFrom; self.provisional=provisional; self.liveState=liveState; self.attention=attention; self.pullRequest=pullRequest; self.owner=owner; self.model=model }
    public init(from decoder: Decoder) throws { let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["id", "projectId", "title", "tags", "createdAtMs", "modifiedAtMs", "pinned", "archived", "lifecycle", "retention", "forkedFrom", "provisional", "liveState", "attention", "pullRequest", "owner", "model"], path: "sessionSummary"); id=try c.decode(SessionId.self, forKey:.id); projectId=try c.decodeIfPresent(ProjectId.self, forKey:.projectId); title=try c.decode(String.self, forKey:.title); tags=try decodeDefault(c,[String].self,forKey:.tags,default:[]); createdAtMs=try c.decode(UInt64.self,forKey:.createdAtMs); modifiedAtMs=try c.decode(UInt64.self,forKey:.modifiedAtMs); pinned=try c.decode(Bool.self,forKey:.pinned); archived=try c.decode(Bool.self,forKey:.archived); lifecycle=try decodeDefault(c,SessionCatalogState.self,forKey:.lifecycle,default:.active); retention=try c.decodeIfPresent(SessionRetention.self,forKey:.retention); forkedFrom=try c.decodeIfPresent(ConversationBranchProvenance.self,forKey:.forkedFrom); provisional=try c.decode(Bool.self,forKey:.provisional); liveState=try c.decode(SessionLiveState.self,forKey:.liveState); attention=try c.decode(AttentionState.self,forKey:.attention); pullRequest=try c.decodeIfPresent(PullRequestSummary.self,forKey:.pullRequest); owner=try c.decode(ActorOwnerState.self,forKey:.owner); model=try c.decode(ModelSelection.self,forKey:.model) }
}

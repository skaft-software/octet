import SwiftUI
import OctetServe

struct SessionDetailView: View {
    @EnvironmentObject private var model: MacOSAppModel
    @FocusState private var composerFocused: Bool

    var body: some View {
        Group {
            if let snapshot = model.currentSession {
                VStack(spacing: 0) {
                    SessionHeader(snapshot: snapshot)
                    Divider()
                    TranscriptView(snapshot: snapshot)
                    Divider()
                    ComposerView(focused: $composerFocused)
                }
            } else {
                VStack(spacing: 12) {
                    Label("No session selected", systemImage: "bubble.left.and.bubble.right")
                        .font(.title3.weight(.semibold))
                    Text("Choose a session from the sidebar or create a new one.")
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    Button("New Session") { model.openCreateSession() }
                        .keyboardShortcut("n", modifiers: [.command])
                        .buttonStyle(.borderedProminent)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityElement(children: .contain)
            }
        }
        .frame(minWidth: 620, minHeight: 500)
    }
}

private struct SessionHeader: View {
    @EnvironmentObject private var model: MacOSAppModel
    let snapshot: SessionSnapshot

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(snapshot.title)
                    .font(.title3.weight(.semibold))
                    .lineLimit(1)
                HStack(spacing: 7) {
                    Text(snapshot.projectID)
                    Text("·")
                    Text(status)
                    if let progress = snapshot.progress {
                        Text("·")
                        ProgressView(value: progress.fraction)
                            .frame(width: 90)
                            .accessibilityLabel("Progress \(Int(progress.fraction * 100)) percent")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
            if let pullRequest = snapshot.pullRequest {
                PullRequestLink(status: pullRequest)
            }
            if isRunning {
                Button("Stop") { model.stopCurrentRun() }
                    .keyboardShortcut(".", modifiers: [.command])
                    .buttonStyle(.bordered)
                    .tint(.red)
                    .accessibilityLabel("Stop current run")
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
        .background(.bar)
        .accessibilityElement(children: .contain)
    }

    private var status: String {
        switch snapshot.status {
        case .idle: return "Idle"
        case .running: return "Running"
        case .waitingForApproval: return "Waiting for approval"
        case .waitingForInput: return "Waiting for input"
        case .completed: return "Completed"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        case .archived: return "Archived"
        }
    }

    private var isRunning: Bool {
        if case .running = snapshot.status { return true }
        return false
    }
}

private struct PullRequestLink: View {
    let status: PullRequestStatus

    var body: some View {
        if let url = status.url {
            Link(destination: url) {
                Label(status.displayName, systemImage: "arrow.up.right.square")
            }
            .font(.caption)
            .help("Open pull request in browser")
            .accessibilityLabel("Open pull request \(status.displayName)")
        } else {
            Label(status.displayName, systemImage: "arrow.up.right.square")
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Pull request \(status.displayName)")
        }
    }
}

private struct TranscriptView: View {
    let snapshot: SessionSnapshot

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    ForEach(snapshot.items) { item in
                        TranscriptItemView(item: item)
                            .id(item.id)
                    }
                    if snapshot.items.isEmpty {
                        Text("No output yet. The host will stream activity here.")
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, minHeight: 180)
                    }
                }
                .padding(18)
                .frame(maxWidth: 900, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .center)
            }
            .scrollIndicators(.automatic)
            .onChange(of: snapshot.items.last?.id) { id in
                guard let id else { return }
                withAnimation(.easeOut(duration: 0.2)) {
                    proxy.scrollTo(id, anchor: .bottom)
                }
            }
            .accessibilityLabel("Session transcript")
        }
    }
}

private struct TranscriptItemView: View {
    @EnvironmentObject private var model: MacOSAppModel
    let item: TranscriptItem

    var body: some View {
        Group {
            switch item.kind {
            case .approval:
                if let approval = item.approval {
                    ApprovalCard(request: approval)
                } else {
                    MessageCard(item: item)
                }
            case .input:
                if let input = item.input {
                    InputRequestCard(request: input)
                } else {
                    MessageCard(item: item)
                }
            case .tool:
                if let tool = item.tool {
                    ToolCard(tool: tool)
                } else {
                    MessageCard(item: item)
                }
            case .user, .assistant, .reasoning, .outcome:
                MessageCard(item: item)
            }
        }
        .accessibilityElement(children: .contain)
    }
}

private struct MessageCard: View {
    let item: TranscriptItem

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack {
                Label(item.title, systemImage: icon)
                    .font(.headline)
                Spacer()
                if let createdAt = item.createdAt {
                    Text(createdAt, style: .time)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
            Text(item.content)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let detail = item.detail, !detail.isEmpty {
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            ReferenceList(sources: item.sources, outputs: item.outputs)
        }
        .padding(12)
        .background(background, in: RoundedRectangle(cornerRadius: 9))
    }

    private var icon: String {
        switch item.kind {
        case .user: return "person"
        case .reasoning: return "brain"
        case .outcome: return "flag.checkered"
        case .assistant: return "sparkles"
        case .tool, .approval, .input: return "text.bubble"
        }
    }
    private var background: Color {
        if case .user = item.kind {
            return Color.accentColor.opacity(0.10)
        }
        return Color(nsColor: .controlBackgroundColor)
    }
}

private struct ToolCard: View {
    let tool: ToolEvidence
    @State private var expanded = true

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 8) {
                if let command = tool.command, !command.isEmpty {
                    Text(command)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color.black.opacity(0.08), in: RoundedRectangle(cornerRadius: 5))
                }
                if let output = tool.output, !output.isEmpty {
                    Text(output)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                if let exitCode = tool.exitCode {
                    Text("Exit code \(exitCode)")
                        .font(.caption)
                        .foregroundStyle(exitCode == 0 ? .green : .red)
                }
                if tool.outputTruncated {
                    Text("Output truncated by host; open the full artifact from the session.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.top, 7)
        } label: {
            HStack {
                Label(tool.name, systemImage: "wrench.and.screwdriver")
                    .font(.headline)
                Spacer()
                Text(stateLabel)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .background(Color.orange.opacity(0.09), in: RoundedRectangle(cornerRadius: 9))
        .accessibilityLabel("Tool \(tool.name), \(stateLabel)")
    }

    private var stateLabel: String {
        switch tool.state {
        case .queued: return "Queued"
        case .running: return "Running"
        case .completed: return "Completed"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        }
    }
}

private struct ApprovalCard: View {
    @EnvironmentObject private var model: MacOSAppModel
    let request: ApprovalRequest

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Approval required", systemImage: "hand.raised")
                .font(.headline)
            Text(request.summary)
                .font(.body.weight(.medium))
            KeyValueLine(label: "Effect", value: request.effect)
            KeyValueLine(label: "Target", value: request.target)
            if !request.risk.isEmpty {
                Text(request.risk)
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            HStack {
                Button("Approve") {
                    model.resolveApproval(request, decision: .approve)
                }
                .buttonStyle(.borderedProminent)
                .disabled(request.resolution != nil)
                Button("Deny", role: .destructive) {
                    model.resolveApproval(request, decision: .deny)
                }
                .disabled(request.resolution != nil)
                if let resolution = request.resolution {
                    Text(resolutionLabel(resolution))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(14)
        .background(Color.orange.opacity(0.13), in: RoundedRectangle(cornerRadius: 9))
        .overlay(RoundedRectangle(cornerRadius: 9).stroke(.orange.opacity(0.45)))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Approval required: \(request.summary)")
    }

    private func resolutionLabel(_ resolution: ApprovalResolutionState) -> String {
        switch resolution {
        case .approved: return "Approved"
        case .denied: return "Denied"
        case .expired: return "Expired"
        }
    }
}

private struct InputRequestCard: View {
    @EnvironmentObject private var model: MacOSAppModel
    let request: UserInputRequest
    @State private var value = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Label("Input requested", systemImage: "text.cursor")
                .font(.headline)
            Text(request.prompt)
            if !request.choices.isEmpty {
                Picker("Choice", selection: $value) {
                    Text("Choose…").tag("")
                    ForEach(request.choices, id: \.self) { choice in
                        Text(choice).tag(choice)
                    }
                }
                .pickerStyle(.menu)
            } else {
                TextField("Response", text: $value)
                    .textFieldStyle(.roundedBorder)
            }
            HStack {
                Button("Submit") {
                    model.resolveInput(request, value: value)
                }
                .buttonStyle(.borderedProminent)
                .disabled(value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || request.resolved)
                if request.resolved {
                    Text("Answered")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(14)
        .background(Color.blue.opacity(0.10), in: RoundedRectangle(cornerRadius: 9))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Input requested: \(request.prompt)")
    }
}

private struct KeyValueLine: View {
    let label: String
    let value: String

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .frame(width: 58, alignment: .leading)
            Text(value)
                .textSelection(.enabled)
        }
    }
}

private struct ReferenceList: View {
    let sources: [SourceReference]
    let outputs: [OutputReference]

    var body: some View {
        if !sources.isEmpty || !outputs.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                if !sources.isEmpty {
                    Text("Sources")
                        .font(.caption.weight(.semibold))
                    ForEach(sources) { source in
                        if let url = source.url {
                            Link(source.title, destination: url)
                                .font(.caption)
                        } else {
                            Text(source.title)
                                .font(.caption)
                        }
                    }
                }
                if !outputs.isEmpty {
                    Text("Outputs")
                        .font(.caption.weight(.semibold))
                        .padding(.top, 3)
                    ForEach(outputs) { output in
                        if let url = output.url {
                            Link(output.title, destination: url)
                                .font(.caption)
                        } else {
                            Text(output.title)
                                .font(.caption)
                        }
                    }
                }
            }
            .foregroundStyle(.secondary)
        }
    }
}

private struct ComposerView: View {
    @EnvironmentObject private var model: MacOSAppModel
    @FocusState.Binding var focused: Bool

    init(focused: FocusState<Bool>.Binding) {
        _focused = focused
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label(modeLabel, systemImage: modeIcon)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                if !model.draft.isEmpty {
                    Button("Clear") { model.draft = "" }
                        .buttonStyle(.borderless)
                }
            }
            HStack(alignment: .bottom, spacing: 10) {
                TextEditor(text: $model.draft)
                    .font(.body)
                    .focused($focused)
                    .frame(minHeight: 62, maxHeight: 130)
                    .padding(5)
                    .overlay(RoundedRectangle(cornerRadius: 7).stroke(.quaternary))
                    .accessibilityLabel("Message composer")
                    .accessibilityHint("Type a prompt or follow-up, then press Command Return")
                Button(model.composerMode.buttonTitle) {
                    model.submitDraft()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: [.command])
                .disabled(model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isDisabled)
                .accessibilityLabel(model.composerMode.buttonTitle)
            }
        }
        .padding(14)
        .background(.bar)
    }

    private var isDisabled: Bool {
        if case .disabled = model.composerMode { return true }
        return false
    }

    private var modeLabel: String {
        switch model.composerMode {
        case .prompt: return "New prompt"
        case .steer: return "Steering active run"
        case .followUp: return "Follow-up"
        case .disabled(let reason): return reason
        }
    }

    private var modeIcon: String {
        switch model.composerMode {
        case .prompt: return "arrow.up.circle"
        case .steer: return "arrow.triangle.turn.up.right.diamond"
        case .followUp: return "arrow.uturn.forward"
        case .disabled: return "lock"
        }
    }
}

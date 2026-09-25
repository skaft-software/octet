import SwiftUI
import OctetServe

struct CreateSessionView: View {
    @EnvironmentObject private var model: MacOSAppModel
    @Environment(\.dismiss) private var dismiss
    @State private var projectID = ""
    @State private var modelID: String?
    @State private var authorityProfileID: String?
    @State private var title = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("New Serve session")
                    .font(.title2.weight(.semibold))
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding()
            Divider()
            Form {
                Section("Workspace") {
                    Picker("Project", selection: $projectID) {
                        Text("Choose a project…").tag("")
                        ForEach(model.bootstrap?.projects ?? []) { project in
                            Text(project.name).tag(project.id)
                        }
                    }
                    .accessibilityLabel("Project")
                    TextField("Optional session title", text: $title)
                }
                Section("Execution") {
                    Picker("Model", selection: $modelID) {
                        Text("Host default").tag(String?.none)
                        ForEach(model.bootstrap?.models ?? []) { availableModel in
                            Text(availableModel.name).tag(Optional(availableModel.id))
                        }
                    }
                    if let profiles = model.bootstrap?.authorityProfiles, !profiles.isEmpty {
                        Picker("Permission profile", selection: $authorityProfileID) {
                            Text("Host default").tag(String?.none)
                            ForEach(profiles) { profile in
                                VStack(alignment: .leading) {
                                    Text(profile.name)
                                    Text(profile.summary)
                                        .font(.caption)
                                }
                                .tag(Optional(profile.id))
                            }
                        }
                        .accessibilityHint("Controls which host-authorized tools the session may use")
                    }
                }
                Section {
                    Text("The host creates the session and remains authoritative. If the connection drops, Octet keeps this form and does not retry a mutation automatically.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
            Divider()
            HStack {
                Spacer()
                Button("Create") {
                    model.createSession(
                        projectID: projectID,
                        modelID: modelID,
                        title: title.isEmpty ? nil : title,
                        authorityProfileID: authorityProfileID
                    )
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(projectID.isEmpty)
            }
            .padding()
        }
        .frame(width: 540, height: 510)
        .onAppear {
            if projectID.isEmpty { projectID = model.bootstrap?.projects.first?.id ?? "" }
        }
        .accessibilityElement(children: .contain)
    }
}

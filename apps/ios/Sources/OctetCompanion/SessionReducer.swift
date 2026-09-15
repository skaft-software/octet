import Foundation

public enum SessionReduceResult: Equatable, Sendable {
    case applied
    case ignored
    case needsSnapshot
}

public enum SessionReducer {
    public static func apply(_ event: WireEventEnvelope, to projection: inout SessionProjection) -> SessionReduceResult {
        guard event.sessionID == projection.id,
              event.cursor.actorGeneration == projection.actorGeneration else {
            return .needsSnapshot
        }
        if event.cursor.sequence <= projection.cursor.sequence {
            return .ignored
        }
        guard event.cursor.sequence == projection.cursor.sequence + 1 else {
            return .needsSnapshot
        }

        var next = projection
        switch event.eventType {
        case "session.stateChanged":
            if let state = event.eventData["state"]?.stringValue {
                next.liveState = state
            }
            next.activeRunID = event.eventData["activeRunId"]?.stringValue
        case "session.settingsChanged":
            if let model = event.eventData["model"]?.objectValue {
                let provider = model["provider"]?.stringValue ?? ""
                let selected = model["model"]?.stringValue ?? ""
                next.modelLabel = [provider, selected].filter { !$0.isEmpty }.joined(separator: " / ")
            }
        case "session.metadataChanged":
            if let title = event.eventData["title"]?.stringValue {
                next.title = SafeText.value(title, limit: 512) ?? next.title
            }
        case "item.started", "item.committed":
            guard let itemValue = event.eventData["item"],
                  let item = decode(WireSessionItem.self, from: itemValue),
                  let entry = item.transcriptEntry() else {
                // A recognized but non-renderable payload remains in the
                // authoritative cursor. It must not become arbitrary UI text.
                break
            }
            upsert(entry, in: &next.items)
        case "item.delta":
            guard let itemID = event.eventData["itemId"]?.stringValue,
                  let delta = event.eventData["delta"]?.objectValue,
                  let deltaType = delta["type"]?.stringValue else {
                return .needsSnapshot
            }
            let data = delta["data"]?.objectValue ?? [:]
            guard let index = next.items.firstIndex(where: { $0.id == itemID }) else {
                return .needsSnapshot
            }
            switch deltaType {
            case "assistantText":
                let append = data["append"]?.stringValue ?? ""
                next.items[index].text = capped(next.items[index].text + append, limit: 128_000)
            case "reasoningText":
                let append = data["append"]?.stringValue ?? ""
                next.items[index].text = capped(next.items[index].text + append, limit: 64_000)
            case "toolActivity":
                guard let activity = data["activity"] else { return .needsSnapshot }
                let payload = WireJSONValue.object([
                    "type": .string("toolCall"),
                    "data": .object(["activity": activity])
                ])
                let item = WireSessionItem(id: itemID, lifecycle: "provisional", payload: payload)
                guard let replacement = item.transcriptEntry() else { return .needsSnapshot }
                next.items[index] = replacement
            default:
                break
            }
        case "item.retracted":
            if let itemID = event.eventData["itemId"]?.stringValue {
                next.items.removeAll { $0.id == itemID }
            }
        case "request.changed":
            guard let requestValue = event.eventData["request"],
                  let request = decode(WirePendingRequest.self, from: requestValue) else {
                return .needsSnapshot
            }
            let requestView = request.view()
            if requestView.isPending {
                upsert(requestView, in: &next.pendingRequests)
            } else {
                next.pendingRequests.removeAll { $0.id == requestView.id }
            }
        case "session.projectionReplaced":
            return .needsSnapshot
        default:
            // Source, artifact, usage, branch, context, and extension events
            // are authoritative but have no safe transcript projection here.
            break
        }

        next.cursor = event.cursor
        projection = next
        return .applied
    }

    private static func decode<T: Decodable>(_ type: T.Type, from value: WireJSONValue) -> T? {
        guard let data = try? JSONEncoder().encode(value) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }

    private static func upsert(_ entry: TranscriptEntry, in items: inout [TranscriptEntry]) {
        if let index = items.firstIndex(where: { $0.id == entry.id }) {
            items[index] = entry
        } else {
            items.append(entry)
        }
    }

    private static func upsert(_ request: PendingRequestView, in requests: inout [PendingRequestView]) {
        if let index = requests.firstIndex(where: { $0.id == request.id }) {
            requests[index] = request
        } else {
            requests.append(request)
        }
    }

    private static func capped(_ value: String, limit: Int) -> String {
        SafeText.value(value, limit: limit) ?? ""
    }
}

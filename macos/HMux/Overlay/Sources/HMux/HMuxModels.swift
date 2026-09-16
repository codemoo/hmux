import Foundation

func hmuxIsValidWorkspaceSourceKey(_ value: String) -> Bool {
	value.utf8.count == 64 && value.utf8.allSatisfy {
		($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102)
	}
}

func hmuxShouldAcceptWorkspaceSource(current: String?, incoming: String?) -> Bool {
	guard let incoming, hmuxIsValidWorkspaceSourceKey(incoming) else { return false }
	return current == nil || current == incoming
}

func hmuxTabAfterClosing(index: Int, remaining: [String], recent: [String]) -> String? {
	if let identity = recent.first(where: { remaining.contains($0) }) { return identity }
	guard !remaining.isEmpty else { return nil }
	return remaining[min(max(index, 0), remaining.count - 1)]
}

struct HMuxAppErrorPayload: Decodable, Equatable {
    let code: String
    let message: String
}

struct HMuxCatalogEnvelope: Decodable {
    let appProtocolVersion: Int
    let ok: Bool
    let data: HMuxCatalog?
    let error: HMuxAppErrorPayload?
}

struct HMuxCatalog: Decodable, Equatable {
    let protocolVersion: Int
    let generatedAt: String
    let sessions: [HMuxSession]
    let appUpdate: HMuxAppUpdate?
	let hostMetrics: HMuxHostMetrics?
	var workspaceSourceKey: String?

	private enum CodingKeys: String, CodingKey {
		case protocolVersion
		case generatedAt
		case sessions
		case appUpdate
		case hostMetrics
		case workspaceSourceKey
	}

	init(from decoder: Decoder) throws {
		let container = try decoder.container(keyedBy: CodingKeys.self)
		protocolVersion = try container.decode(Int.self, forKey: .protocolVersion)
		generatedAt = try container.decode(String.self, forKey: .generatedAt)
		guard container.contains(.sessions) else {
			throw DecodingError.keyNotFound(
				CodingKeys.sessions,
				DecodingError.Context(
					codingPath: container.codingPath,
					debugDescription: "The HMux catalog requires a sessions field."
				)
			)
		}
		sessions = try container.decodeNil(forKey: .sessions)
			? []
			: container.decode([HMuxSession].self, forKey: .sessions)
		appUpdate = try container.decodeIfPresent(HMuxAppUpdate.self, forKey: .appUpdate)
		// Optional telemetry cannot invalidate otherwise valid session data.
		hostMetrics = try? container.decodeIfPresent(HMuxHostMetrics.self, forKey: .hostMetrics)
		workspaceSourceKey = try container.decodeIfPresent(String.self, forKey: .workspaceSourceKey)
		if let workspaceSourceKey, !hmuxIsValidWorkspaceSourceKey(workspaceSourceKey) {
			throw DecodingError.dataCorruptedError(forKey: .workspaceSourceKey, in: container, debugDescription: "Invalid workspace source key.")
		}
	}
}

struct HMuxHostMetrics: Decodable, Equatable, Sendable {
	let observedAt: String
	let cpuPercent: Double?
	let gpuPercent: Double?
	let memoryUsedBytes: UInt64?
	let memoryTotalBytes: UInt64?

	private enum CodingKeys: String, CodingKey {
		case observedAt
		case cpuPercent
		case gpuPercent
		case memoryUsedBytes
		case memoryTotalBytes
	}

	init(from decoder: Decoder) throws {
		let container = try decoder.container(keyedBy: CodingKeys.self)
		observedAt = try container.decode(String.self, forKey: .observedAt)
		guard HMuxCatalogStamp(observedAt) != nil else {
			throw DecodingError.dataCorruptedError(
				forKey: .observedAt,
				in: container,
				debugDescription: "Host metrics requires a valid UTC RFC3339 timestamp."
			)
		}

		cpuPercent = try Self.decodePercent(.cpuPercent, from: container)
		gpuPercent = try Self.decodePercent(.gpuPercent, from: container)
		memoryUsedBytes = try container.decodeIfPresent(UInt64.self, forKey: .memoryUsedBytes)
		memoryTotalBytes = try container.decodeIfPresent(UInt64.self, forKey: .memoryTotalBytes)

		switch (memoryUsedBytes, memoryTotalBytes) {
		case (nil, nil):
			break
		case let (used?, total?) where total > 0 && total <= (1 << 60) && used <= total:
			break
		default:
			throw DecodingError.dataCorrupted(
				DecodingError.Context(
					codingPath: container.codingPath,
					debugDescription: "Host memory metrics must be a valid used/total byte pair."
				)
			)
		}
		guard cpuPercent != nil || gpuPercent != nil || memoryTotalBytes != nil else {
			throw DecodingError.dataCorrupted(
				DecodingError.Context(
					codingPath: container.codingPath,
					debugDescription: "Host metrics requires at least one sampled value."
				)
			)
		}
	}

	var observedStamp: HMuxCatalogStamp {
		// Decoding validates this invariant before a value enters the app.
		HMuxCatalogStamp(observedAt)!
	}

	var observedDate: Date {
		Date(
			timeIntervalSince1970: TimeInterval(observedStamp.secondsSince1970) +
				TimeInterval(observedStamp.nanoseconds) / 1_000_000_000
		)
	}

	private static func decodePercent(
		_ key: CodingKeys,
		from container: KeyedDecodingContainer<CodingKeys>
	) throws -> Double? {
		guard let value = try container.decodeIfPresent(Double.self, forKey: key) else { return nil }
		guard value.isFinite, (0...100).contains(value) else {
			throw DecodingError.dataCorruptedError(
				forKey: key,
				in: container,
				debugDescription: "Host utilization percentages must be finite values from 0 through 100."
			)
		}
		return value
	}
}

struct HMuxCatalogStamp: Comparable, Equatable {
	let secondsSince1970: Int64
	let nanoseconds: UInt32

	init?(_ value: String) {
		let bytes = Array(value.utf8)
		guard (20...30).contains(bytes.count),
		      bytes[4] == 45, bytes[7] == 45, bytes[10] == 84,
		      bytes[13] == 58, bytes[16] == 58, bytes.last == 90 else { return nil }

		func decimal(_ range: Range<Int>) -> Int? {
			guard range.upperBound <= bytes.count else { return nil }
			var result = 0
			for byte in bytes[range] {
				guard byte >= 48, byte <= 57 else { return nil }
				result = result * 10 + Int(byte - 48)
			}
			return result
		}

		guard let year = decimal(0..<4), let month = decimal(5..<7), let day = decimal(8..<10),
		      let hour = decimal(11..<13), let minute = decimal(14..<16), let second = decimal(17..<19),
		      (1...9999).contains(year), (1...12).contains(month),
		      (0...23).contains(hour), (0...59).contains(minute), (0...59).contains(second) else { return nil }

		var fraction: UInt32 = 0
		if bytes.count > 20 {
			guard bytes[19] == 46 else { return nil }
			let digits = bytes.count - 21
			guard (1...9).contains(digits), let parsed = decimal(20..<(bytes.count - 1)) else { return nil }
			fraction = UInt32(parsed)
			for _ in digits..<9 { fraction *= 10 }
		}

		var calendar = Calendar(identifier: .gregorian)
		calendar.timeZone = TimeZone(secondsFromGMT: 0)!
		guard let date = calendar.date(from: DateComponents(
			timeZone: calendar.timeZone,
			year: year, month: month, day: day,
			hour: hour, minute: minute, second: second
		)), calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date) ==
			DateComponents(year: year, month: month, day: day, hour: hour, minute: minute, second: second) else {
			return nil
		}
		secondsSince1970 = Int64(date.timeIntervalSince1970.rounded())
		nanoseconds = fraction
	}

	static func < (left: HMuxCatalogStamp, right: HMuxCatalogStamp) -> Bool {
		if left.secondsSince1970 != right.secondsSince1970 {
			return left.secondsSince1970 < right.secondsSince1970
		}
		return left.nanoseconds < right.nanoseconds
	}
}

func hmuxShouldAcceptCatalogStamp(_ incoming: HMuxCatalogStamp, after current: HMuxCatalogStamp?) -> Bool {
	current.map { incoming >= $0 } ?? true
}

struct HMuxAppUpdate: Decodable, Equatable {
    let version: String
    let installedAt: String
}

struct HMuxProfile: Decodable, Equatable, Identifiable, Sendable {
	let id: String
	let label: String
	let tags: [String]

	private enum CodingKeys: String, CodingKey { case id, label, tags }

	init(id: String, label: String, tags: [String]) {
		self.id = id
		self.label = label
		self.tags = tags
	}

	init(from decoder: Decoder) throws {
		let container = try decoder.container(keyedBy: CodingKeys.self)
		id = try container.decode(String.self, forKey: .id)
		label = try container.decode(String.self, forKey: .label)
		tags = try container.decodeIfPresent([String].self, forKey: .tags) ?? []
	}
}

struct HMuxSessionCreation: Decodable, Equatable, Sendable {
	let session: HMuxSessionIdentity
	let reused: Bool
}

struct HMuxSession: Decodable, Equatable, Identifiable {
    let id: String
    let name: String
    var alias: String?
    var hidden: Bool?
    let createdAt: Int64
    let activityAt: Int64
    let attachedClients: Int
    let windowCount: Int
    let windowNames: [String]
    let activeWindow: String
    let currentPath: String
    let currentCommand: String
    let profile: String?
    let label: String?
    let tags: [String]?
    let kind: String?
    let runtime: String?
    let model: String?
    let state: String?
    let process: String?
    let workingSince: Int64?
    let workflow: HMuxWorkflowSummary?
    let workflows: [HMuxWorkflow]?
    let hostAlias: String?
    let width: Int?
    let height: Int?
    var restoredFrom: HMuxSessionIdentity? = nil

    var identity: String { "\(id):\(createdAt)" }
    var isHidden: Bool { hidden == true }
    var displayName: String { alias?.isEmpty == false ? alias! : name }
    var detailCommand: String {
        let candidate = process?.isEmpty == false ? process! : currentCommand
        return candidate.trimmingCharacters(in: .whitespacesAndNewlines)
    }
    var modelLabel: String? {
        guard let model = model?.trimmingCharacters(in: .whitespacesAndNewlines), !model.isEmpty else { return nil }
        return model
    }

    var sidebarProjection: HMuxSidebarProjection {
        HMuxSidebarProjection(
            identity: identity,
            displayName: displayName,
            currentPath: currentPath,
            detailCommand: detailCommand,
            model: modelLabel,
            runtime: runtime,
            state: state,
            attachedClients: attachedClients,
            hidden: isHidden,
            tags: tags ?? []
        )
    }
}

func hmuxCanonicalAlias(_ alias: String?) -> String? {
    guard let alias, !alias.isEmpty else { return nil }
    return alias
}

func hmuxSession(_ session: HMuxSession, replacingAlias alias: String?) -> HMuxSession {
    var projected = session
    projected.alias = hmuxCanonicalAlias(alias)
    return projected
}

func hmuxSession(_ session: HMuxSession, replacingHidden hidden: Bool) -> HMuxSession {
	var projected = session
	projected.hidden = hidden
	return projected
}

func hmuxSessionCatalogLessThan(_ left: HMuxSession, _ right: HMuxSession) -> Bool {
	let locale = Locale(identifier: "en_US_POSIX")
	let leftName = left.displayName.folding(
		options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: locale
	).lowercased(with: locale)
	let rightName = right.displayName.folding(
		options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: locale
	).lowercased(with: locale)
	let comparison = leftName.compare(
		rightName,
		options: [.numeric, .literal],
		range: nil,
		locale: locale
	)
	if comparison != .orderedSame { return comparison == .orderedAscending }
	return left.identity < right.identity
}

struct HMuxMutationFence<Value: Equatable>: Equatable {
	let token: UInt64
	let previous: Value
	let desired: Value
	var isAcknowledged = false

	func project(_ authoritative: Value) -> Value { desired }

}

struct HMuxSidebarProjection: Equatable {
    let identity: String
    let displayName: String
    let currentPath: String
    let detailCommand: String
    let model: String?
    let runtime: String?
    let state: String?
    let attachedClients: Int
    let hidden: Bool
    let tags: [String]
}

struct HMuxAttentionSummary: Equatable {
    let approvals: Int
    let inputs: Int
    let failures: Int

    init(sessions: [HMuxSession]) {
        approvals = sessions.filter { $0.state == "waiting_approval" }.count
        inputs = sessions.filter { $0.state == "waiting_input" }.count
        failures = sessions.filter { $0.state == "failed" }.count
    }

    var total: Int { approvals + inputs + failures }
    var isEmpty: Bool { total == 0 }
}

struct HMuxSessionIdentity: Codable, Equatable, Sendable {
    let id: String
    let createdAt: Int64

	init(id: String, createdAt: Int64) {
		self.id = id
		self.createdAt = createdAt
	}

    init(session: HMuxSession) {
        id = session.id
        createdAt = session.createdAt
    }
}

// Home alone declares recovery lineage. Names and recycled tmux IDs never
// authorize a visual tab to attach to a different lifetime.
func hmuxRestoredSession(_ reference: HMuxSessionIdentity, in sessions: [HMuxSession]) -> HMuxSession? {
    guard hmuxIsValidSessionIdentity(reference) else { return nil }
    let candidates = sessions.filter {
        hmuxIsValidSessionIdentity(HMuxSessionIdentity(session: $0)) &&
        (HMuxSessionIdentity(session: $0) == reference || $0.restoredFrom == reference)
    }
    guard candidates.count == 1, let candidate = candidates.first,
          sessions.filter({ $0.id == candidate.id }).count == 1 else { return nil }
    return candidate
}

func hmuxIsValidProfileID(_ value: String) -> Bool {
	guard (1...63).contains(value.utf8.count),
	      let first = value.utf8.first,
	      first >= 97, first <= 122 else { return false }
	return value.utf8.allSatisfy { byte in
		(byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57) || byte == 45
	}
}

func hmuxIsValidSessionName(_ value: String) -> Bool {
	guard (1...80).contains(value.unicodeScalars.count) else { return false }
	return value.unicodeScalars.allSatisfy { scalar in
		CharacterSet.letters.contains(scalar) || CharacterSet.decimalDigits.contains(scalar) ||
			scalar.value == 0x20 || scalar.value == 0x5f || scalar.value == 0x2d
	}
}

func hmuxIsValidSessionIdentity(_ identity: HMuxSessionIdentity) -> Bool {
	guard identity.createdAt > 0,
	      (2...13).contains(identity.id.utf8.count),
	      identity.id.utf8.first == 36 else { return false }
	return identity.id.utf8.dropFirst().allSatisfy { byte in byte >= 48 && byte <= 57 }
}

func hmuxSessionMatches(_ session: HMuxSession, query: String, now: Date = Date()) -> Bool {
	let terms = query
		.split(whereSeparator: \.isWhitespace)
		.map { hmuxNormalizedSearchText(String($0)) }
		.filter { !$0.isEmpty }
	guard !terms.isEmpty else { return true }

	let age = max(0, Int64(now.timeIntervalSince1970) - session.activityAt)
	var activityTerms = [String(session.activityAt)]
	switch age {
	case ..<60:
		activityTerms += ["now", "recent", "less than one minute", "<1m"]
	case ..<3_600:
		activityTerms += ["recent", "minutes", "\(age / 60)m"]
	case ..<86_400:
		activityTerms += ["today", "hours", "\(age / 3_600)h"]
	default:
		activityTerms += ["days", "\(age / 86_400)d"]
	}

	var rawFields: [String] = []
	rawFields.reserveCapacity(24 + session.windowNames.count + (session.tags?.count ?? 0))
	rawFields.append(contentsOf: [
		session.displayName, session.name, session.alias ?? "", session.label ?? "",
		session.profile ?? "", session.kind ?? "", session.currentPath,
		session.currentCommand, session.detailCommand, session.activeWindow,
	])
	rawFields.append(contentsOf: [
		session.modelLabel ?? "", session.runtime ?? "", session.state ?? "",
		session.process ?? "", session.hostAlias ?? "",
	])
	rawFields.append(contentsOf: session.windowNames)
	rawFields.append(contentsOf: session.tags ?? [])
	rawFields.append(session.attachedClients > 0 ? "attached connected" : "detached")
	rawFields.append("\(session.attachedClients) clients")
	rawFields.append("\(session.windowCount) windows")
	rawFields.append(activityTerms.joined(separator: " "))
	let fields = rawFields.map { hmuxNormalizedSearchText($0) }

	return terms.allSatisfy { term in
		fields.contains { hmuxFuzzySearchTerm(term, matches: $0) }
	}
}

private func hmuxNormalizedSearchText(_ value: String) -> String {
	value.folding(
		options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive],
		locale: Locale(identifier: "en_US_POSIX")
	).lowercased()
}

private func hmuxFuzzySearchTerm(_ term: String, matches field: String) -> Bool {
	if field.contains(term) { return true }
	guard term.count >= 3 else { return false }
	var remaining = field[...]
	for character in term {
		guard let index = remaining.firstIndex(of: character) else { return false }
		remaining = remaining[remaining.index(after: index)...]
	}
	return true
}

enum HMuxSessionFilter: String, CaseIterable, Identifiable {
    case all = "All"
    case active = "Active"
    case attention = "Attention"

    var id: String { rawValue }
}

struct HMuxWorkflowSummary: Decodable, Equatable {
    let running: Int
    let waitingApproval: Int
    let waitingInput: Int
    let completed: Int
    let failed: Int
    let interrupted: Int
    let stale: Int
    let updatedAt: Int64
}

struct HMuxWorkflow: Decodable, Equatable, Identifiable {
    let id: String
    let source: String
    let sessionId: String?
    let turnId: String?
    let status: String
    let model: String?
    let startedAt: Int64
    let updatedAt: Int64
    let endedAt: Int64?
    let nodes: [HMuxWorkflowNode]
}

struct HMuxWorkflowNode: Decodable, Equatable, Identifiable {
    let id: String
    let parentId: String?
    let type: String
    let provider: String
    let status: String
    let startedAt: Int64
    let updatedAt: Int64
    let endedAt: Int64?
}

func hmuxWorkflowNodeDepths(_ nodes: [HMuxWorkflowNode]) -> [String: Int] {
    var parents: [String: String] = [:]
    for candidate in nodes where parents[candidate.id] == nil {
        parents[candidate.id] = candidate.parentId ?? ""
    }

	var depths: [String: Int] = [:]
	depths.reserveCapacity(parents.count)
	for node in nodes where depths[node.id] == nil {
		var parentID = node.parentId
		var seen = Set([node.id])
		var depth = 0
		while let candidate = parentID,
		      let next = parents[candidate],
		      depth < 6,
		      seen.insert(candidate).inserted {
			depth += 1
			parentID = next.isEmpty ? nil : next
		}
		depths[node.id] = depth
	}
	return depths
}

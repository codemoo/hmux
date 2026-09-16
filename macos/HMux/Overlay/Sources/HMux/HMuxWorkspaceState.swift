import Foundation

enum HMuxWorkspaceStateError: Error, Equatable {
	case invalidSourceKey
	case invalidSnapshot
	case unsupportedVersion
	case tooManyOpenSessions
	case oversizedData
	case corruptData
}

struct HMuxWorkspaceSnapshot: Codable, Equatable, Sendable {
	static let currentVersion = 1
	static let maximumOpenSessions = 32
	static let maximumEncodedBytes = 64 * 1024

	let version: Int
	let sourceKey: String
	let openSessions: [HMuxSessionIdentity]
	let selectedSession: HMuxSessionIdentity?
	let sidebarVisible: Bool?
	let inspectorVisible: Bool?

	private enum CodingKeys: String, CodingKey {
		case version
		case sourceKey
		case openSessions
		case selectedSession
		case sidebarVisible
		case inspectorVisible
	}

	static func capture(
		sourceKey: String,
		openSessions: [HMuxSessionIdentity],
		selectedSession: HMuxSessionIdentity?,
		sidebarVisible: Bool? = nil,
		inspectorVisible: Bool? = nil
	) throws -> HMuxWorkspaceSnapshot {
		let snapshot = HMuxWorkspaceSnapshot(
			version: currentVersion,
			sourceKey: sourceKey,
			openSessions: openSessions,
			selectedSession: selectedSession,
			sidebarVisible: sidebarVisible,
			inspectorVisible: inspectorVisible
		)
		try snapshot.validate()
		return snapshot
	}

	private init(
		version: Int,
		sourceKey: String,
		openSessions: [HMuxSessionIdentity],
		selectedSession: HMuxSessionIdentity?,
		sidebarVisible: Bool?,
		inspectorVisible: Bool?
	) {
		self.version = version
		self.sourceKey = sourceKey
		self.openSessions = openSessions
		self.selectedSession = selectedSession
		self.sidebarVisible = sidebarVisible
		self.inspectorVisible = inspectorVisible
	}

	init(from decoder: Decoder) throws {
		let container = try decoder.container(keyedBy: CodingKeys.self)
		version = try container.decode(Int.self, forKey: .version)
		sourceKey = try container.decode(String.self, forKey: .sourceKey)
		openSessions = try container.decode([HMuxSessionIdentity].self, forKey: .openSessions)
		selectedSession = try container.decodeIfPresent(HMuxSessionIdentity.self, forKey: .selectedSession)
		sidebarVisible = try container.decodeIfPresent(Bool.self, forKey: .sidebarVisible)
		inspectorVisible = try container.decodeIfPresent(Bool.self, forKey: .inspectorVisible)
		try validate()
	}

	func encode(to encoder: Encoder) throws {
		try validate()
		var container = encoder.container(keyedBy: CodingKeys.self)
		try container.encode(version, forKey: .version)
		try container.encode(sourceKey, forKey: .sourceKey)
		try container.encode(openSessions, forKey: .openSessions)
		try container.encodeIfPresent(selectedSession, forKey: .selectedSession)
		try container.encodeIfPresent(sidebarVisible, forKey: .sidebarVisible)
		try container.encodeIfPresent(inspectorVisible, forKey: .inspectorVisible)
	}

	func restorePlan(
		sourceKey currentSourceKey: String,
		currentSessions: [HMuxSession]
	) -> HMuxWorkspaceRestorePlan? {
		guard (try? validate()) != nil,
		      hmuxWorkspaceSourceKeyIsValid(currentSourceKey),
		      sourceKey == currentSourceKey else { return nil }

		let sessions = openSessions.compactMap { reference -> HMuxSession? in
			return hmuxRestoredSession(reference, in: currentSessions)
		}
		let survivingIdentities = sessions.map(HMuxSessionIdentity.init(session:))
		let repairedSelection = selectedSession.flatMap { selected in
			guard let restored = hmuxRestoredSession(selected, in: sessions) else { return nil as HMuxSessionIdentity? }
			return HMuxSessionIdentity(session: restored)
		} ?? survivingIdentities.first

		return HMuxWorkspaceRestorePlan(
			sessions: sessions,
			selectedSession: repairedSelection,
			sidebarVisible: sidebarVisible,
			inspectorVisible: inspectorVisible
		)
	}

	fileprivate func validate() throws {
		guard version == Self.currentVersion else {
			throw HMuxWorkspaceStateError.unsupportedVersion
		}
		guard hmuxWorkspaceSourceKeyIsValid(sourceKey) else {
			throw HMuxWorkspaceStateError.invalidSourceKey
		}
		guard openSessions.count <= Self.maximumOpenSessions else {
			throw HMuxWorkspaceStateError.tooManyOpenSessions
		}

		var sessionIDs = Set<String>()
		for reference in openSessions {
			guard hmuxIsValidSessionIdentity(reference),
			      sessionIDs.insert(reference.id).inserted else {
				throw HMuxWorkspaceStateError.invalidSnapshot
			}
		}
		if let selectedSession {
			guard hmuxIsValidSessionIdentity(selectedSession),
			      openSessions.contains(selectedSession) else {
				throw HMuxWorkspaceStateError.invalidSnapshot
			}
		}
	}
}

struct HMuxWorkspaceRestorePlan: Equatable {
	let sessions: [HMuxSession]
	let selectedSession: HMuxSessionIdentity?
	let sidebarVisible: Bool?
	let inspectorVisible: Bool?
}

struct HMuxWorkspacePersistence {
	static let defaultsKey = "dev.hmux.app.workspace-snapshot"

	private let defaults: UserDefaults

	init(defaults: UserDefaults = .standard) {
		self.defaults = defaults
	}

	func save(_ snapshot: HMuxWorkspaceSnapshot) throws {
		try snapshot.validate()
		let encoder = JSONEncoder()
		encoder.outputFormatting = [.sortedKeys]
		let data = try encoder.encode(snapshot)
		guard data.count <= HMuxWorkspaceSnapshot.maximumEncodedBytes else {
			throw HMuxWorkspaceStateError.oversizedData
		}
		defaults.set(data, forKey: Self.defaultsKey)
	}

	func load(sourceKey: String) throws -> HMuxWorkspaceSnapshot? {
		guard hmuxWorkspaceSourceKeyIsValid(sourceKey) else {
			throw HMuxWorkspaceStateError.invalidSourceKey
		}
		guard let object = defaults.object(forKey: Self.defaultsKey) else { return nil }
		guard let data = object as? Data else {
			throw HMuxWorkspaceStateError.corruptData
		}
		guard data.count <= HMuxWorkspaceSnapshot.maximumEncodedBytes else {
			throw HMuxWorkspaceStateError.oversizedData
		}

		let snapshot: HMuxWorkspaceSnapshot
		do {
			snapshot = try JSONDecoder().decode(HMuxWorkspaceSnapshot.self, from: data)
		} catch let error as HMuxWorkspaceStateError {
			throw error
		} catch {
			throw HMuxWorkspaceStateError.corruptData
		}
		guard snapshot.sourceKey == sourceKey else { return nil }
		return snapshot
	}
}

private func hmuxWorkspaceSourceKeyIsValid(_ value: String) -> Bool {
	guard value.utf8.count == 64 else { return false }
	return value.utf8.allSatisfy { byte in
		(byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
	}
}

// The Home store merges changes for every native and web client.
struct HMuxSharedWorkspace: Decodable, Sendable {
    var conflict: String? = nil
    let version: Int
    let initialized: Bool
    let revision: UInt64
    let tabs: [HMuxSessionIdentity]
    let selected: HMuxSessionIdentity?

    func validate() throws {
        guard version == 1, tabs.count <= 32,
              tabs.allSatisfy(hmuxIsValidSessionIdentity),
              Set(tabs.map(\.id)).count == tabs.count,
              selected == nil || tabs.contains(selected!) else {
            throw HMuxWorkspaceStateError.invalidSnapshot
        }
    }
}

struct HMuxSharedWorkspaceChange: Encodable, Sendable {
    let operationId: String
    let revision: UInt64
    let base: [HMuxSessionIdentity]
    let tabs: [HMuxSessionIdentity]
    let selected: HMuxSessionIdentity?
}

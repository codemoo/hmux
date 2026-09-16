import Foundation

private enum HMuxWorkspaceStateSmokeError: Error {
	case failed(String)
}

@main
struct HMuxWorkspaceStateSmoke {
	static func main() throws {
		let suiteName = "dev.hmux.tests.workspace-state.\(UUID().uuidString)"
		guard let defaults = UserDefaults(suiteName: suiteName) else {
			throw HMuxWorkspaceStateSmokeError.failed("defaults suite")
		}
		defer { defaults.removePersistentDomain(forName: suiteName) }

		let persistence = HMuxWorkspacePersistence(defaults: defaults)
		let sourceA = String(repeating: "a", count: 64)
		let sourceB = String(repeating: "b", count: 64)
		let first = HMuxSessionIdentity(id: "$1", createdAt: 1_700_000_001)
		let second = HMuxSessionIdentity(id: "$2", createdAt: 1_700_000_002)
		let missing = HMuxSessionIdentity(id: "$3", createdAt: 1_700_000_003)


        let shared = HMuxSharedWorkspace(version: 1, initialized: true, revision: 3, tabs: [second, first], selected: first)
        try shared.validate()
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let change = HMuxSharedWorkspaceChange(operationId: "0123456789abcdef", revision: 3, base: [first], tabs: [second, first], selected: first)
        let wire = try JSONSerialization.jsonObject(with: encoder.encode(change)) as! [String: Any]
        guard wire["operation_id"] as? String == change.operationId,
              wire["revision"] as? Int == 3 else {
            throw HMuxWorkspaceStateSmokeError.failed("shared workspace wire keys")
        }
        do {
            try HMuxSharedWorkspace(version: 1, initialized: true, revision: 0, tabs: [first, first], selected: nil).validate()
            throw HMuxWorkspaceStateSmokeError.failed("duplicate shared tabs accepted")
        } catch HMuxWorkspaceStateError.invalidSnapshot { }
		let snapshot = try HMuxWorkspaceSnapshot.capture(
			sourceKey: sourceA,
			openSessions: [second, first],
			selectedSession: first,
			sidebarVisible: true,
			inspectorVisible: false
		)
		try persistence.save(snapshot)
		guard try persistence.load(sourceKey: sourceA) == snapshot,
		      let savedData = defaults.data(forKey: HMuxWorkspacePersistence.defaultsKey),
		      savedData.count <= HMuxWorkspaceSnapshot.maximumEncodedBytes else {
			throw HMuxWorkspaceStateSmokeError.failed("valid round trip")
		}
		let savedText = String(decoding: savedData, as: UTF8.self)
		guard !savedText.contains("name"),
		      !savedText.contains("path"),
		      !savedText.contains("pane"),
		      !savedText.contains("credential") else {
			throw HMuxWorkspaceStateSmokeError.failed("persisted sensitive fields")
		}

		let storedBeforeMismatch = defaults.object(forKey: HMuxWorkspacePersistence.defaultsKey) as? Data
		guard try persistence.load(sourceKey: sourceB) == nil,
		      defaults.object(forKey: HMuxWorkspacePersistence.defaultsKey) as? Data == storedBeforeMismatch,
		      try persistence.load(sourceKey: sourceA) == snapshot else {
			throw HMuxWorkspaceStateSmokeError.failed("source mismatch mutation")
		}

		let firstSession = try makeSession(identity: first, name: "sensitive-first", path: "/private/first")
		let secondSession = try makeSession(identity: second, name: "sensitive-second", path: "/private/second")
		guard let plan = snapshot.restorePlan(
			sourceKey: sourceA,
			currentSessions: [firstSession, secondSession]
		), plan.sessions.map(\.identity) == [secondSession.identity, firstSession.identity],
		   plan.selectedSession == first,
		   plan.sidebarVisible == true,
		   plan.inspectorVisible == false,
		   snapshot.restorePlan(sourceKey: sourceB, currentSessions: [firstSession, secondSession]) == nil else {
			throw HMuxWorkspaceStateSmokeError.failed("restore order or selection")
		}

		let repairSnapshot = try HMuxWorkspaceSnapshot.capture(
			sourceKey: sourceA,
			openSessions: [second, first, missing],
			selectedSession: missing
		)
		guard let repairPlan = repairSnapshot.restorePlan(
			sourceKey: sourceA,
			currentSessions: [firstSession, secondSession]
		), repairPlan.sessions.map(\.identity) == [secondSession.identity, firstSession.identity],
		   repairPlan.selectedSession == second else {
			throw HMuxWorkspaceStateSmokeError.failed("missing selection repair")
		}

		let oldIdentity = HMuxSessionIdentity(id: "$9", createdAt: 1_700_000_009)
		let recycledIdentity = HMuxSessionIdentity(id: "$9", createdAt: 1_700_000_099)
		let oldSnapshot = try HMuxWorkspaceSnapshot.capture(
			sourceKey: sourceA,
			openSessions: [oldIdentity],
			selectedSession: oldIdentity
		)
		let recycledSession = try makeSession(identity: recycledIdentity, name: "recycled", path: "/private/recycled")
		guard let recycledPlan = oldSnapshot.restorePlan(sourceKey: sourceA, currentSessions: [recycledSession]),
		      recycledPlan.sessions.isEmpty,
		      recycledPlan.selectedSession == nil else {
			throw HMuxWorkspaceStateSmokeError.failed("recycled ID restored")
		}
		let oldSession = try makeSession(identity: oldIdentity, name: "old", path: "/private/old")
		guard let ambiguousPlan = oldSnapshot.restorePlan(
			sourceKey: sourceA,
			currentSessions: [oldSession, recycledSession]
		), ambiguousPlan.sessions.isEmpty,
		   ambiguousPlan.selectedSession == nil else {
			throw HMuxWorkspaceStateSmokeError.failed("ambiguous catalog ID restored")
		}

        let recovered = try makeSession(identity: recycledIdentity, name: "recovered", path: "/synthetic", restoredFrom: oldIdentity)
        guard let recoveredPlan = oldSnapshot.restorePlan(sourceKey: sourceA, currentSessions: [recovered]),
              recoveredPlan.sessions.map(\.identity) == [recovered.identity],
              recoveredPlan.selectedSession == recycledIdentity else {
            throw HMuxWorkspaceStateSmokeError.failed("explicit Home recovery lineage not restored")
        }
        guard hmuxRestoredSession(oldIdentity, in: [oldSession, recovered]) == nil,
              hmuxRestoredSession(oldIdentity, in: [recovered, recovered]) == nil else {
            throw HMuxWorkspaceStateSmokeError.failed("ambiguous recovery lineage accepted")
        }

		let empty = try HMuxWorkspaceSnapshot.capture(
			sourceKey: sourceA,
			openSessions: [],
			selectedSession: nil
		)
		try persistence.save(empty)
		guard try persistence.load(sourceKey: sourceA) == empty,
		      let emptyPlan = empty.restorePlan(sourceKey: sourceA, currentSessions: [firstSession]),
		      emptyPlan.sessions.isEmpty,
		      emptyPlan.selectedSession == nil else {
			throw HMuxWorkspaceStateSmokeError.failed("empty round trip")
		}

		try expect(.invalidSourceKey) {
			_ = try HMuxWorkspaceSnapshot.capture(
				sourceKey: String(repeating: "A", count: 64),
				openSessions: [],
				selectedSession: nil
			)
		}
		try expect(.invalidSnapshot) {
			_ = try HMuxWorkspaceSnapshot.capture(
				sourceKey: sourceA,
				openSessions: [HMuxSessionIdentity(id: "$bad", createdAt: 1)],
				selectedSession: nil
			)
		}
		try expect(.invalidSnapshot) {
			_ = try HMuxWorkspaceSnapshot.capture(
				sourceKey: sourceA,
				openSessions: [first, first],
				selectedSession: first
			)
		}
		try expect(.invalidSnapshot) {
			_ = try HMuxWorkspaceSnapshot.capture(
				sourceKey: sourceA,
				openSessions: [oldIdentity, recycledIdentity],
				selectedSession: oldIdentity
			)
		}
		try expect(.invalidSnapshot) {
			_ = try HMuxWorkspaceSnapshot.capture(
				sourceKey: sourceA,
				openSessions: [first],
				selectedSession: second
			)
		}
		try expect(.tooManyOpenSessions) {
			let references = (1...33).map {
				HMuxSessionIdentity(id: "$\($0)", createdAt: Int64(1_700_001_000 + $0))
			}
			_ = try HMuxWorkspaceSnapshot.capture(
				sourceKey: sourceA,
				openSessions: references,
				selectedSession: references[0]
			)
		}

		defaults.set(Data("{\"version\":2,\"sourceKey\":\"\(sourceA)\",\"openSessions\":[]}".utf8),
		             forKey: HMuxWorkspacePersistence.defaultsKey)
		try expect(.unsupportedVersion) { _ = try persistence.load(sourceKey: sourceA) }

		defaults.set(Data("{".utf8), forKey: HMuxWorkspacePersistence.defaultsKey)
		try expect(.corruptData) { _ = try persistence.load(sourceKey: sourceA) }

		defaults.set("not data", forKey: HMuxWorkspacePersistence.defaultsKey)
		try expect(.corruptData) { _ = try persistence.load(sourceKey: sourceA) }

		defaults.set(Data(repeating: 0, count: HMuxWorkspaceSnapshot.maximumEncodedBytes + 1),
		             forKey: HMuxWorkspacePersistence.defaultsKey)
		try expect(.oversizedData) { _ = try persistence.load(sourceKey: sourceA) }

		print("workspace-state-ok")
	}

	private static func expect(
		_ expected: HMuxWorkspaceStateError,
		operation: () throws -> Void
	) throws {
		do {
			try operation()
			throw HMuxWorkspaceStateSmokeError.failed("accepted \(expected)")
		} catch let error as HMuxWorkspaceStateError where error == expected {
			return
		}
	}

	private static func makeSession(
		identity: HMuxSessionIdentity,
		name: String,
		path: String,
        restoredFrom: HMuxSessionIdentity? = nil
	) throws -> HMuxSession {
		var object: [String: Any] = [
			"id": identity.id,
			"name": name,
			"createdAt": identity.createdAt,
			"activityAt": identity.createdAt,
			"attachedClients": 0,
			"windowCount": 1,
			"windowNames": ["shell"],
			"activeWindow": "shell",
			"currentPath": path,
			"currentCommand": "zsh",
		]
        if let restoredFrom { object["restoredFrom"] = ["id": restoredFrom.id, "createdAt": restoredFrom.createdAt] }
		let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
		return try JSONDecoder().decode(HMuxSession.self, from: data)
	}
}

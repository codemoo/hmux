import Foundation

@main
struct HMuxCatalogMutationSmoke {
	static func main() throws {
		guard CommandLine.arguments.count == 2 else { throw HMuxBackendError.commandFailed }
		let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
		let catalog = try HMuxBackend.decodeCatalog(data)
		guard let base = catalog.sessions.first else { throw HMuxBackendError.invalidProtocol }

		guard let oldStamp = HMuxCatalogStamp("2026-09-08T00:00:00.000000001Z"),
		      let confirmationStamp = HMuxCatalogStamp("2026-09-08T00:00:00.000000002Z"),
		      let remoteEditStamp = HMuxCatalogStamp("2026-09-08T00:00:01Z"),
		      oldStamp < confirmationStamp, confirmationStamp < remoteEditStamp,
		      hmuxShouldAcceptCatalogStamp(oldStamp, after: nil),
		      hmuxShouldAcceptCatalogStamp(oldStamp, after: oldStamp),
		      hmuxShouldAcceptCatalogStamp(confirmationStamp, after: oldStamp),
		      !hmuxShouldAcceptCatalogStamp(oldStamp, after: confirmationStamp),
		      hmuxShouldAcceptCatalogStamp(remoteEditStamp, after: confirmationStamp),
		      HMuxCatalogStamp("2026-02-29T00:00:00Z") == nil,
		      HMuxCatalogStamp("not-a-time") == nil else {
			throw HMuxBackendError.invalidProtocol
		}

		guard hmuxSession(base, replacingHidden: true).isHidden,
		      !hmuxSession(hmuxSession(base, replacingHidden: true), replacingHidden: false).isHidden else {
			throw HMuxBackendError.invalidProtocol
		}

		func fixture(
			id: String, name: String, alias: String?, createdAt: Int64,
			activityAt: Int64, attachedClients: Int
		) -> HMuxSession {
			HMuxSession(
				id: id, name: name, alias: alias, hidden: false,
				createdAt: createdAt, activityAt: activityAt, attachedClients: attachedClients,
				windowCount: base.windowCount, windowNames: base.windowNames,
				activeWindow: base.activeWindow, currentPath: base.currentPath,
				currentCommand: base.currentCommand, profile: base.profile,
				label: base.label, tags: base.tags, kind: base.kind,
				runtime: base.runtime, model: base.model, state: base.state,
				process: base.process, workingSince: base.workingSince,
				workflow: base.workflow, workflows: base.workflows,
				hostAlias: base.hostAlias, width: base.width, height: base.height
			)
		}
		let unsorted = [
			fixture(id: "$5", name: "zulu", alias: "Alpha 10", createdAt: 5, activityAt: 500, attachedClients: 2),
			fixture(id: "$3", name: "alpha 2", alias: nil, createdAt: 3, activityAt: 900, attachedClients: 0),
			fixture(id: "$4", name: "Beta", alias: nil, createdAt: 4, activityAt: 100, attachedClients: 4),
			fixture(id: "$2", name: "unused", alias: "alpha 1", createdAt: 2, activityAt: 700, attachedClients: 0),
			fixture(id: "$1", name: "unused", alias: "ÁLPHA 2", createdAt: 1, activityAt: 50, attachedClients: 8),
		]
		let sorted = unsorted.sorted(by: hmuxSessionCatalogLessThan)
		guard sorted.map(\.identity) == ["$2:2", "$1:1", "$3:3", "$5:5", "$4:4"] else {
			throw HMuxBackendError.invalidProtocol
		}
		let activityChanged = unsorted.reversed().enumerated().map { index, session in
			fixture(
				id: session.id, name: session.name, alias: session.alias, createdAt: session.createdAt,
				activityAt: Int64(index), attachedClients: 10 - index
			)
		}.sorted(by: hmuxSessionCatalogLessThan)
		guard activityChanged.map(\.identity) == sorted.map(\.identity) else {
			throw HMuxBackendError.invalidProtocol
		}

		let recycled = fixture(
			id: "$9", name: "replacement", alias: nil, createdAt: 99,
			activityAt: 0, attachedClients: 0
		)
		let priorIdentity = "$9:98"
		let identityFences = [priorIdentity: HMuxMutationFence(token: 1, previous: Optional("A"), desired: Optional("B"))]
		guard recycled.identity != priorIdentity, identityFences[recycled.identity] == nil else {
			throw HMuxBackendError.invalidProtocol
		}
		print("catalog-ordering-ok timestamps alias-first natural-order identity")
	}
}

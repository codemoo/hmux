import Foundation

@main
struct HMuxModelSmoke {
    static func main() throws {
		let source = String(repeating: "a", count: 64)
		let otherSource = String(repeating: "b", count: 64)
		guard hmuxShouldAcceptWorkspaceSource(current: nil, incoming: source),
		      hmuxShouldAcceptWorkspaceSource(current: source, incoming: source),
		      !hmuxShouldAcceptWorkspaceSource(current: nil, incoming: nil),
		      !hmuxShouldAcceptWorkspaceSource(current: source, incoming: nil),
		      !hmuxShouldAcceptWorkspaceSource(current: source, incoming: otherSource),
		      !hmuxShouldAcceptWorkspaceSource(current: nil, incoming: ""),
		      !hmuxShouldAcceptWorkspaceSource(current: nil, incoming: source.uppercased()),
		      !hmuxShouldAcceptWorkspaceSource(current: "invalid", incoming: "invalid") else {
			throw HMuxBackendError.invalidProtocol
		}
		guard hmuxTabAfterClosing(index: 1, remaining: ["a", "c"], recent: ["c", "a"]) == "c",
		      hmuxTabAfterClosing(index: 1, remaining: ["a", "c"], recent: ["gone", "a"]) == "a",
		      hmuxTabAfterClosing(index: 5, remaining: ["a", "c"], recent: []) == "c",
		      hmuxTabAfterClosing(index: 0, remaining: [], recent: ["gone"]) == nil,
		      hmuxIsValidWorkspaceSourceKey(String(repeating: "a", count: 64)),
		      !hmuxIsValidWorkspaceSourceKey(String(repeating: "A", count: 64)) else {
			throw HMuxBackendError.invalidProtocol
		}
        guard CommandLine.arguments.count == 3 else { throw HMuxBackendError.commandFailed }
        let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        let catalog = try HMuxBackend.decodeCatalog(data)
        guard catalog.protocolVersion == 1,
              catalog.sessions.count == 1,
              catalog.sessions[0].identity == "$101:1787410000",
              catalog.sessions[0].workflows?.first?.nodes.count == 1 else {
            throw HMuxBackendError.invalidProtocol
        }

		let emptyCatalogPrefix = """
		{"app_protocol_version":1,"ok":true,"data":{"protocol_version":1,"generated_at":"2026-09-08T00:00:00Z",
		"""
		for sessionsValue in ["null", "[]"] {
			let emptyCatalog = try HMuxBackend.decodeCatalog(Data((emptyCatalogPrefix + "\"sessions\":\(sessionsValue)}}").utf8))
			guard emptyCatalog.sessions.isEmpty else { throw HMuxBackendError.invalidProtocol }
		}
		for invalidCatalog in [
			emptyCatalogPrefix + "\"other\":[]}}",
			emptyCatalogPrefix + "\"sessions\":{}}}",
		] {
			do {
				_ = try HMuxBackend.decodeCatalog(Data(invalidCatalog.utf8))
				throw HMuxBackendError.commandFailed
			} catch is DecodingError {
				// Expected: absence and wrong types remain protocol failures.
			}
		}

		let profilesResponse = """
		{"app_protocol_version":1,"ok":true,"data":{"profiles":[{"id":"codex","label":"Codex","tags":["ai","agent"]},{"id":"shell","label":"Shell"}]}}
		"""
		let profiles = try HMuxBackend.decodeProfiles(Data(profilesResponse.utf8))
		guard profiles == [
			HMuxProfile(id: "codex", label: "Codex", tags: ["ai", "agent"]),
			HMuxProfile(id: "shell", label: "Shell", tags: []),
		], hmuxIsValidProfileID("codex-2"), !hmuxIsValidProfileID("Codex"),
		   hmuxIsValidSessionName("연구 세션_2"), !hmuxIsValidSessionName("bad/name"),
		   !hmuxIsValidSessionName(String(repeating: "a", count: 81)) else {
			throw HMuxBackendError.invalidProtocol
		}

		let unnamedRequest = try HMuxBackend.encodeCreateRequest(profileID: "codex", name: "")
		let namedRequest = try HMuxBackend.encodeCreateRequest(profileID: "codex", name: "연구 세션_2")
		let unnamedObject = try JSONSerialization.jsonObject(with: unnamedRequest) as? [String: Any]
		let namedObject = try JSONSerialization.jsonObject(with: namedRequest) as? [String: Any]
		guard unnamedObject?["profile_id"] as? String == "codex",
		      unnamedObject?["name"] == nil,
		      namedObject?["profile_id"] as? String == "codex",
		      namedObject?["name"] as? String == "연구 세션_2" else {
			throw HMuxBackendError.invalidProtocol
		}
		for invalidRequest in [
			("Codex", ""),
			("codex", "bad/name"),
			("codex", String(repeating: "a", count: 81)),
		] {
			do {
				_ = try HMuxBackend.encodeCreateRequest(profileID: invalidRequest.0, name: invalidRequest.1)
				throw HMuxBackendError.commandFailed
			} catch HMuxBackendError.invalidProtocol {
				// Expected.
			}
		}

		let createResponse = """
		{"app_protocol_version":1,"ok":true,"data":{"session":{"id":"$42","created_at":1700000000},"reused":false}}
		"""
		let creation = try HMuxBackend.decodeCreateResult(Data(createResponse.utf8))
		guard creation == HMuxSessionCreation(
			session: HMuxSessionIdentity(id: "$42", createdAt: 1_700_000_000), reused: false
		), hmuxIsValidSessionIdentity(creation.session),
		   !hmuxIsValidSessionIdentity(HMuxSessionIdentity(id: "$42;bad", createdAt: 1_700_000_000)) else {
			throw HMuxBackendError.invalidProtocol
		}
		for invalidResponse in [
			profilesResponse.replacingOccurrences(of: "\"id\":\"shell\"", with: "\"id\":\"codex\""),
			profilesResponse.replacingOccurrences(of: "\"id\":\"codex\"", with: "\"id\":\"Codex\""),
			profilesResponse.replacingOccurrences(of: "\"label\":\"Codex\"", with: "\"label\":\"Co\\u202edex\""),
		] {
			do {
				_ = try HMuxBackend.decodeProfiles(Data(invalidResponse.utf8))
				throw HMuxBackendError.commandFailed
			} catch HMuxBackendError.invalidProtocol {
				// Expected.
			}
		}
		for invalidResponse in [
			createResponse.replacingOccurrences(of: "\"$42\"", with: "\"42\""),
			createResponse.replacingOccurrences(of: "1700000000", with: "0"),
		] {
			do {
				_ = try HMuxBackend.decodeCreateResult(Data(invalidResponse.utf8))
				throw HMuxBackendError.commandFailed
			} catch HMuxBackendError.invalidProtocol {
				// Expected.
			}
		}

		let nativeUpdate = HMuxAppUpdate(version: "9.9.9", installedAt: "2026-08-23T00:00:00Z")
		guard hmuxMergedAppUpdate(current: nativeUpdate, incoming: nil, mode: .webSocket) == nativeUpdate,
		      hmuxMergedAppUpdate(current: nativeUpdate, incoming: nil, mode: .pollingFallback) == nil else {
			throw HMuxBackendError.invalidProtocol
		}
		guard hmuxShouldAcceptCatalogResult(
			generation: 2, currentGeneration: 2, activeWebSocketGeneration: 2, mode: .webSocket,
			startingRevision: 4, currentRevision: 5
		), !hmuxShouldAcceptCatalogResult(
			generation: 2, currentGeneration: 2, activeWebSocketGeneration: 2, mode: .pollingFallback,
			startingRevision: 4, currentRevision: 4
		), !hmuxShouldAcceptCatalogResult(
			generation: 1, currentGeneration: 2, activeWebSocketGeneration: nil, mode: .pollingFallback,
			startingRevision: 4, currentRevision: 4
		), !hmuxShouldAcceptCatalogResult(
			generation: 2, currentGeneration: 2, activeWebSocketGeneration: nil, mode: .pollingFallback,
			startingRevision: 4, currentRevision: 5
		) else {
			throw HMuxBackendError.invalidProtocol
		}
		guard hmuxNextCatalogStreamFailureCount(previous: 2, connectionLifetime: 30) == 3,
		      hmuxNextCatalogStreamFailureCount(previous: 2, connectionLifetime: 120) == 1 else {
			throw HMuxBackendError.invalidProtocol
		}
		let outageStart = Date(timeIntervalSince1970: 100)
		guard !hmuxCatalogOutageShouldBeOffline(
			hasReceivedCatalog: true, disconnectedAt: outageStart,
			now: Date(timeIntervalSince1970: 119)
		), hmuxCatalogOutageShouldBeOffline(
			hasReceivedCatalog: true, disconnectedAt: outageStart,
			now: Date(timeIntervalSince1970: 120)
		), hmuxCatalogOutageShouldBeOffline(
			hasReceivedCatalog: false, disconnectedAt: outageStart,
			now: outageStart
		) else {
			throw HMuxBackendError.invalidProtocol
		}

		let searchFixture = String(decoding: data, as: UTF8.self)
			.replacingOccurrences(of: "\"name\": \"hmux-e2e-codex\"", with: "\"name\": \"base\"")
			.replacingOccurrences(of: "\"alias\": \"Codex app spike\"", with: "\"alias\": \"alpha\"")
			.replacingOccurrences(of: "\"profile\": \"codex\"", with: "\"profile\": \"gpu\"")
			.replacingOccurrences(of: "\"label\": \"Codex\"", with: "\"label\": \"Research\"")
			.replacingOccurrences(of: "\"kind\": \"agent\"", with: "\"kind\": \"claude\"")
			.replacingOccurrences(of: "\"tags\": [\"agent\"]", with: "\"tags\": [\"urgent\"]")
		let searchCatalog = try HMuxBackend.decodeCatalog(Data(searchFixture.utf8))
		let searchable = searchCatalog.sessions[0]
		let renamed = hmuxSession(searchable, replacingAlias: "renamed locally")
		let restored = hmuxSession(renamed, replacingAlias: "")
		guard renamed.displayName == "renamed locally",
		      searchable.displayName == "alpha",
		      restored.alias == nil,
		      restored.displayName == restored.name else {
			throw HMuxBackendError.invalidProtocol
		}
		guard hmuxSessionMatches(searchable, query: "gpu"),
		      hmuxSessionMatches(searchable, query: "Research"),
		      hmuxSessionMatches(searchable, query: "claude"),
		      hmuxSessionMatches(searchable, query: "alpha urgent"),
		      hmuxSessionMatches(searchable, query: "rserch"),
		      !hmuxSessionMatches(searchable, query: "missing term") else {
			throw HMuxBackendError.invalidProtocol
		}

        let timestampOnly = String(decoding: data, as: UTF8.self)
            .replacingOccurrences(of: "\"activity_at\": 1787410300", with: "\"activity_at\": 1787410301")
            .replacingOccurrences(of: "\"updated_at\": 1787410300", with: "\"updated_at\": 1787410301")
        let timestampCatalog = try HMuxBackend.decodeCatalog(Data(timestampOnly.utf8))
        guard catalog.sessions[0] != timestampCatalog.sessions[0],
              catalog.sessions[0].sidebarProjection == timestampCatalog.sessions[0].sidebarProjection else {
            throw HMuxBackendError.invalidProtocol
        }

        let visibleChange = timestampOnly.replacingOccurrences(
            of: "\"current_path\": \"/tmp/hmux-e2e-project\"",
            with: "\"current_path\": \"/tmp/hmux-e2e-project-2\""
        )
        let visibleCatalog = try HMuxBackend.decodeCatalog(Data(visibleChange.utf8))
        guard catalog.sessions[0].sidebarProjection != visibleCatalog.sessions[0].sidebarProjection else {
            throw HMuxBackendError.invalidProtocol
        }

		let requestID = "00112233445566778899aabbccddeeff"
		let stageID = "ffeeddccbbaa99887766554433221100"
		let stageResponse = """
		{"app_protocol_version":1,"ok":true,"data":{"protocol_version":1,"request_id":"\(requestID)","stage_id":"\(stageID)","session":{"id":"$7","created_at":1700000000},"expires_at_unix":1700086400,"files":[{"index":0,"path":"/Users/home/Library/Caches/hmux/staged-files-v1/1700086400-\(stageID)/file-0001.png","size":10,"sha256":"\(String(repeating: "a", count: 64))"},{"index":1,"path":"/Users/home/Library/Caches/hmux/staged-files-v1/1700086400-\(stageID)/file-0002","size":20,"sha256":"\(String(repeating: "b", count: 64))"}]}}
		"""
		let stageSession = HMuxSessionIdentity(id: "$7", createdAt: 1_700_000_000)
		let stageResult = try HMuxBackend.decodeFileStageResult(
			Data(stageResponse.utf8),
			expectedRequestID: requestID,
			expectedSession: stageSession,
			expectedExtensions: ["png", nil]
		)
		let pasteText = try hmuxFileStagePasteText(stageResult)
		guard stageResult.files.count == 2,
		      !pasteText.contains("\n"),
		      pasteText.hasPrefix("'/Users/home/Library/Caches/hmux/staged-files-v1/"),
		      try hmuxPOSIXQuote("/Users/o'brien/file") == "'/Users/o'\\''brien/file'" else {
			throw HMuxBackendError.invalidFileStage
		}
		for malicious in [
			stageResponse.replacingOccurrences(of: "\"files\":", with: "\"unknown\":true,\"files\":"),
			stageResponse.replacingOccurrences(of: "staged-files-v1", with: "other-root"),
			stageResponse.replacingOccurrences(of: String(repeating: "a", count: 64), with: String(repeating: "z", count: 64)),
			stageResponse.replacingOccurrences(of: "\"index\":0", with: "\"index\":1"),
		] {
			do {
				_ = try HMuxBackend.decodeFileStageResult(
					Data(malicious.utf8),
					expectedRequestID: requestID,
					expectedSession: stageSession,
					expectedExtensions: ["png", nil]
				)
				throw HMuxBackendError.invalidProtocol
			} catch HMuxBackendError.invalidFileStage {
				// Expected.
			} catch DecodingError.dataCorrupted {
				// Also an expected strict decode failure.
			}
		}
		do {
			_ = try hmuxPOSIXQuote("/tmp/unsafe\npath")
			throw HMuxBackendError.invalidProtocol
		} catch HMuxBackendError.invalidFileStage {
			// Expected.
		}
		guard hmuxShouldAutoPasteFileStage(
			targetMatches: true, tabSelected: true, epochMatches: true,
			appActive: true, keyWindow: true, hasSurfaceModel: true
		), !hmuxShouldAutoPasteFileStage(
			targetMatches: true, tabSelected: false, epochMatches: true,
			appActive: true, keyWindow: true, hasSurfaceModel: true
		), !hmuxShouldAutoPasteFileStage(
			targetMatches: true, tabSelected: true, epochMatches: false,
			appActive: true, keyWindow: true, hasSurfaceModel: true
		), !hmuxShouldAutoPasteFileStage(
			targetMatches: false, tabSelected: true, epochMatches: true,
			appActive: true, keyWindow: true, hasSurfaceModel: true
		), hmuxCanPasteReadyFileStage(
			targetMatches: true, tabSelected: true,
			appActive: true, keyWindow: true, hasSurfaceModel: true
		), !hmuxCanPasteReadyFileStage(
			targetMatches: true, tabSelected: true,
			appActive: false, keyWindow: true, hasSurfaceModel: true
		) else {
			throw HMuxBackendError.invalidFileStage
		}

        var bad = String(decoding: data, as: UTF8.self)
        bad = bad.replacingOccurrences(of: "\"app_protocol_version\": 1", with: "\"app_protocol_version\": 999")
        do {
            _ = try HMuxBackend.decodeCatalog(Data(bad.utf8))
            throw HMuxBackendError.invalidProtocol
        } catch HMuxBackendError.invalidProtocol {
            // Expected.
        }

        let uiData = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2]))
        let uiCatalog = try HMuxBackend.decodeCatalog(uiData)
        let attention = HMuxAttentionSummary(sessions: uiCatalog.sessions)
        guard uiCatalog.sessions.count == 4,
              uiCatalog.sessions[0].hostAlias == "home",
              uiCatalog.sessions[0].width == 142,
              uiCatalog.sessions[0].height == 42,
              uiCatalog.sessions.filter({ $0.state == "failed" }).count == 1,
              attention.approvals == 1,
              attention.inputs == 0,
              attention.failures == 1,
              attention.total == 2,
              uiCatalog.sessions[0].detailCommand == "codex",
              uiCatalog.sessions[0].modelLabel == "gpt-5.6-sol" else {
            throw HMuxBackendError.invalidProtocol
        }

        let root = HMuxWorkflowNode(
            id: "root", parentId: "agent", type: "root", provider: "codex",
            status: "running", startedAt: 1, updatedAt: 1, endedAt: nil
        )
        let agent = HMuxWorkflowNode(
            id: "agent", parentId: "root", type: "agent", provider: "codex",
            status: "running", startedAt: 1, updatedAt: 1, endedAt: nil
        )
        let duplicate = HMuxWorkflowNode(
            id: "root", parentId: nil, type: "duplicate", provider: "codex",
            status: "running", startedAt: 1, updatedAt: 1, endedAt: nil
        )
        let malformedNodes = [root, agent, duplicate]
		let malformedDepths = hmuxWorkflowNodeDepths(malformedNodes)
		guard malformedDepths[root.id, default: 0] <= 6,
		      malformedDepths[agent.id, default: 0] <= 6 else {
            throw HMuxBackendError.invalidProtocol
        }
    }
}

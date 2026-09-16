import Foundation
import Darwin

private final class HMuxWorkspaceSourceBinding: @unchecked Sendable {
	static let shared = HMuxWorkspaceSourceBinding()
	private let lock = NSLock()
	private var key: String?
	func read() -> String? { lock.lock(); defer { lock.unlock() }; return key }
	func bind(_ value: String?) { lock.lock(); defer { lock.unlock() }; key = value }
}

enum HMuxBackendError: LocalizedError {
    case executableUnavailable
    case terminalUnavailable
    case oversizedResponse
    case commandFailed
	case commandTimedOut
    case invalidProtocol
	case invalidFileStage
	case streamUnsupported
	case invalidStreamBootstrap
	case streamSequenceGap
	case streamLivenessTimeout
    case backend(String)

    var errorDescription: String? {
        switch self {
        case .executableUnavailable: return "The bundled hmux backend is unavailable."
        case .terminalUnavailable: return "The Ghostty terminal runtime is unavailable."
        case .oversizedResponse: return "The hmux backend response exceeded its size limit."
        case .commandFailed: return "The hmux backend command failed."
		case .commandTimedOut: return "The hmux backend command timed out."
        case .invalidProtocol: return "The hmux app protocol is incompatible."
		case .invalidFileStage: return "The dropped files could not be staged safely."
		case .streamUnsupported: return "The Home agent does not support catalog streaming."
		case .invalidStreamBootstrap: return "The HMux catalog stream could not be authenticated."
		case .streamSequenceGap: return "The HMux catalog stream lost synchronization."
		case .streamLivenessTimeout: return "The HMux catalog stream stopped responding."
        case .backend(let message): return message
        }
    }
}

struct HMuxBackend {
	static func bindWorkspaceSource(_ sourceKey: String?) {
		HMuxWorkspaceSourceBinding.shared.bind(sourceKey)
	}
    static let appProtocolVersion = 1
    static let backendProtocolVersion = 1
    static let maximumResponseBytes = 32 * 1024 * 1024
	static let maximumFileStageResponseBytes = 64 * 1024
	static let catalogTimeout: TimeInterval = 15
	static let mutationTimeout: TimeInterval = 30
	static let nativeUpdateTimeout: TimeInterval = 5 * 60

    static func loadCatalog() async throws -> HMuxCatalog {
#if DEBUG
        if let fixture = ProcessInfo.processInfo.environment["HMUX_CATALOG_FIXTURE"], !fixture.isEmpty {
            return try decodeCatalog(try readFixture(at: fixture))
        }
#endif
		let response = try await runBackend(
			arguments: ["app", "catalog"],
			maximumBytes: maximumResponseBytes,
			timeout: catalogTimeout
		)
		guard response.exit.exitedNormally, response.exit.status == 0 else {
			if let envelope = try? decodeEnvelope(response.data), let error = envelope.error {
				throw HMuxBackendError.backend(error.message)
			}
			throw HMuxBackendError.commandFailed
		}
		return try decodeCatalog(response.data)
    }


    static func sharedWorkspace(change: HMuxSharedWorkspaceChange?) async throws -> HMuxSharedWorkspace {
        struct Request: Encodable { let change: HMuxSharedWorkspaceChange? }
        struct Envelope: Decodable {
            let appProtocolVersion: Int
            let ok: Bool
            let data: HMuxSharedWorkspace?
            let error: HMuxAppErrorPayload?
        }
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let response = try await runBackend(
            arguments: ["app", "workspace"], input: try encoder.encode(Request(change: change)),
            maximumBytes: 64 * 1024, timeout: 15
        )
        let envelope = try JSONDecoder.hmuxDecoder.decode(Envelope.self, from: response.data)
        guard envelope.appProtocolVersion == appProtocolVersion else { throw HMuxBackendError.invalidProtocol }
        guard envelope.ok, envelope.error == nil, response.exit.exitedNormally, response.exit.status == 0,
              let value = envelope.data else {
            throw HMuxBackendError.backend(envelope.error?.message ?? "Shared tabs unavailable.")
        }
        try value.validate()
        return value
    }

	static func loadConversation(session: HMuxSessionIdentity) async throws -> HMuxConversation {
        struct Request: Encodable { let session: HMuxSessionIdentity }
        struct Envelope: Decodable {
            let appProtocolVersion: Int
            let ok: Bool
            let data: HMuxConversation?
            let error: HMuxAppErrorPayload?
        }
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let response = try await runBackend(
            arguments: ["app", "conversation"], input: try encoder.encode(Request(session: session)),
            maximumBytes: 2 * 1024 * 1024, timeout: 15
        )
        let envelope = try JSONDecoder.hmuxDecoder.decode(Envelope.self, from: response.data)
        guard envelope.appProtocolVersion == appProtocolVersion else { throw HMuxBackendError.invalidProtocol }
        guard envelope.ok, envelope.error == nil, response.exit.exitedNormally, response.exit.status == 0 else {
            throw HMuxBackendError.backend(envelope.error?.message ?? "Conversation unavailable.")
        }
        guard let value = envelope.data, value.sessionID == session.id, value.createdAt == session.createdAt else {
            throw HMuxBackendError.invalidProtocol
        }
        return value
    }

	static func loadProfiles() async throws -> [HMuxProfile] {
		let response = try await runBackend(
			arguments: ["app", "profiles"],
			maximumBytes: maximumResponseBytes,
			timeout: catalogTimeout
		)
		guard response.exit.exitedNormally, response.exit.status == 0 else {
			if let envelope = try? JSONDecoder.hmuxDecoder.decode(HMuxProfilesEnvelope.self, from: response.data),
			   let error = envelope.error {
				throw HMuxBackendError.backend(error.message)
			}
			throw HMuxBackendError.commandFailed
		}
		return try decodeProfiles(response.data)
	}

	static func createSession(profileID: String, name: String) async throws -> HMuxSessionCreation {
		let requestData = try encodeCreateRequest(profileID: profileID, name: name)
		let response = try await runBackend(
			arguments: ["app", "create"],
			input: requestData,
			maximumBytes: maximumResponseBytes,
			timeout: mutationTimeout
		)
		let result: HMuxSessionCreation
		do {
			result = try decodeCreateResult(response.data)
		} catch HMuxBackendError.backend(let message) {
			throw HMuxBackendError.backend(message)
		} catch {
			guard response.exit.exitedNormally, response.exit.status == 0 else {
				throw HMuxBackendError.commandFailed
			}
			throw error
		}
		guard response.exit.exitedNormally, response.exit.status == 0 else {
			throw HMuxBackendError.commandFailed
		}
		return result
	}

    static func setAlias(_ alias: String, for session: HMuxSession) async throws {
		try await performMutation(
            command: "alias-set",
            request: HMuxAliasRequest(session: HMuxSessionIdentity(session: session), alias: alias)
        )
    }

    static func setHidden(_ hidden: Bool, for session: HMuxSession) async throws {
		try await performMutation(
            command: "hidden-set",
            request: HMuxHiddenRequest(session: HMuxSessionIdentity(session: session), hidden: hidden)
        )
    }

    static func terminate(_ session: HMuxSession) async throws {
		try await performMutation(
            command: "terminate",
            request: HMuxTerminateRequest(session: HMuxSessionIdentity(session: session), confirmed: true)
        )
    }

	static func stageFiles(
		_ urls: [URL],
		for session: HMuxSessionIdentity,
		requestID: String
	) async throws -> HMuxFileStageResult {
		guard hmuxIsLowercaseHex(requestID, count: 32),
		      !session.id.isEmpty,
		      session.createdAt > 0,
		      (1...16).contains(urls.count),
		      urls.allSatisfy(\.isFileURL) else {
			throw HMuxBackendError.invalidFileStage
		}
		let paths = urls.map(\.path)
		guard paths.allSatisfy({ path in
			path.hasPrefix("/") && path.utf8.count <= 4096 && !hmuxContainsControlCharacter(path)
		}) else {
			throw HMuxBackendError.invalidFileStage
		}
		let extensions = paths.map(hmuxSafeFileStageExtension)
		let request = HMuxFileStageRequest(requestID: requestID, session: session, paths: paths)
		let encoder = JSONEncoder()
		encoder.keyEncodingStrategy = .convertToSnakeCase
		let requestData = try encoder.encode(request)
		guard requestData.count <= 64 * 1024 else { throw HMuxBackendError.invalidFileStage }

		try Task.checkCancellation()
		let runtime = try HMuxFileStageProcessRuntime.start()
		do {
			return try await withTaskCancellationHandler {
				try Task.checkCancellation()
				try await runtime.sendRequest(requestData)
				try Task.checkCancellation()
				var responseData = Data()
				for try await byte in runtime.output.bytes {
					try Task.checkCancellation()
					guard responseData.count < maximumFileStageResponseBytes else {
						throw HMuxBackendError.oversizedResponse
					}
					responseData.append(byte)
				}
				let exit = await runtime.waitForExit()
				try Task.checkCancellation()
				let envelope = try decodeFileStageEnvelope(responseData)
				guard envelope.appProtocolVersion == appProtocolVersion else {
					throw HMuxBackendError.invalidProtocol
				}
				guard exit.exitedNormally, exit.status == 0, envelope.ok, let result = envelope.data else {
					throw HMuxBackendError.backend(
						envelope.error?.message ?? "HMux could not stage the dropped files."
					)
				}
				try validateFileStageResult(
					result,
					expectedRequestID: requestID,
					expectedSession: session,
					expectedExtensions: extensions
				)
				_ = try hmuxFileStagePasteText(result)
				return result
			} onCancel: {
				runtime.requestStop()
			}
		} catch {
			await runtime.shutdown()
			throw error
		}
	}

    static func checkForNativeUpdate() async throws -> HMuxAppUpdate? {
		let response = try await runBackend(
			arguments: ["app", "update-native"],
			maximumBytes: maximumResponseBytes,
			timeout: nativeUpdateTimeout
		)
		let envelope = try JSONDecoder.hmuxDecoder.decode(HMuxNativeUpdateEnvelope.self, from: response.data)
        guard envelope.appProtocolVersion == appProtocolVersion else { throw HMuxBackendError.invalidProtocol }
		guard response.exit.exitedNormally, response.exit.status == 0, envelope.ok else {
            throw HMuxBackendError.backend(envelope.error?.message ?? "HMux automatic update failed.")
        }
        return envelope.data?.appUpdate
    }

    static func helperDirectory() throws -> URL {
        let directory = Bundle.main.bundleURL
            .appendingPathComponent("Contents", isDirectory: true)
            .appendingPathComponent("Helpers", isDirectory: true)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory.path, isDirectory: &isDirectory), isDirectory.boolValue else {
            throw HMuxBackendError.executableUnavailable
        }
        return directory
    }

	static func backendExecutable() throws -> URL {
#if DEBUG
        if let override = ProcessInfo.processInfo.environment["HMUX_EXECUTABLE"], override.hasPrefix("/") {
            let candidate = URL(fileURLWithPath: override)
            if isExecutableRegularFile(candidate) { return candidate }
        }
#endif
        let candidate = try helperDirectory().appendingPathComponent("hmux", isDirectory: false)
        guard isExecutableRegularFile(candidate) else { throw HMuxBackendError.executableUnavailable }
        return candidate
    }

    private static func performMutation<Request: Encodable>(command: String, request: Request) async throws {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let requestData = try encoder.encode(request)
        guard requestData.count <= 64 * 1024 else { throw HMuxBackendError.oversizedResponse }

		let response = try await runBackend(
			arguments: ["app", command],
			input: requestData,
			maximumBytes: maximumResponseBytes,
			timeout: mutationTimeout
		)
		let envelope = try JSONDecoder.hmuxDecoder.decode(HMuxMutationEnvelope.self, from: response.data)
        guard envelope.appProtocolVersion == appProtocolVersion else { throw HMuxBackendError.invalidProtocol }
		guard response.exit.exitedNormally, response.exit.status == 0, envelope.ok else {
            throw HMuxBackendError.backend(envelope.error?.message ?? "HMux session update failed.")
        }
    }

	private static func runBackend(
		arguments: [String],
		input: Data? = nil,
		maximumBytes: Int,
		timeout: TimeInterval
	) async throws -> (data: Data, exit: HMuxProcessExit) {
		try Task.checkCancellation()
		let runtime = try HMuxBackendProcessRuntime.start(arguments: arguments)
		do {
			return try await withTaskCancellationHandler {
				try Task.checkCancellation()
				let timeoutTask = Task.detached(priority: .utility) {
					let nanoseconds = UInt64(max(1, timeout) * 1_000_000_000)
					do { try await Task.sleep(nanoseconds: nanoseconds) }
					catch { return }
					runtime.requestStop(timedOut: true)
				}
				defer { timeoutTask.cancel() }
				if let input { try await runtime.send(input) }
				else { runtime.closeInput() }
				var data = Data()
				do {
					for try await byte in runtime.output.bytes {
						try Task.checkCancellation()
						guard data.count < maximumBytes else {
							runtime.requestStop(timedOut: false)
							throw HMuxBackendError.oversizedResponse
						}
						data.append(byte)
					}
				} catch {
					if runtime.didTimeOut { throw HMuxBackendError.commandTimedOut }
					throw error
				}
				let exit = await runtime.waitForExit()
				try Task.checkCancellation()
				if runtime.didTimeOut { throw HMuxBackendError.commandTimedOut }
				return (data, exit)
			} onCancel: {
				runtime.requestStop(timedOut: false)
			}
		} catch {
			await runtime.shutdown()
			throw error
		}
	}

    private static func isExecutableRegularFile(_ url: URL) -> Bool {
        guard let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey]),
              values.isRegularFile == true,
              values.isSymbolicLink != true else { return false }
        return FileManager.default.isExecutableFile(atPath: url.path)
    }

    static func appEnvironmentMetadata() -> [String: String] {
		var metadata: [String: String] = [:]
		if let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String, !version.isEmpty {
			metadata["HMUX_APP_BUNDLE_PATH"] = Bundle.main.bundleURL.path
			metadata["HMUX_APP_VERSION"] = version
		}
		if let key = HMuxWorkspaceSourceBinding.shared.read() { metadata["HMUX_WORKSPACE_SOURCE_KEY"] = key }
		return metadata
    }

	static func backendEnvironment() -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        for key in ["TMUX", "TMUX_PANE", "HMUX_LAUNCHER", "HMUX_LAUNCHER_ID", "HMUX_WORKSPACE_SOURCE_KEY"] {
            environment.removeValue(forKey: key)
        }
        environment.merge(appEnvironmentMetadata()) { _, appValue in appValue }
        return environment
    }

    static func decodeCatalog(_ data: Data) throws -> HMuxCatalog {
        let envelope = try decodeEnvelope(data)
        guard envelope.appProtocolVersion == appProtocolVersion else { throw HMuxBackendError.invalidProtocol }
        guard envelope.ok else { throw HMuxBackendError.backend(envelope.error?.message ?? "HMux app request failed.") }
        guard let catalog = envelope.data, catalog.protocolVersion == backendProtocolVersion else {
            throw HMuxBackendError.invalidProtocol
        }
        return catalog
    }

    static func decodeEnvelope(_ data: Data) throws -> HMuxCatalogEnvelope {
        try JSONDecoder.hmuxDecoder.decode(HMuxCatalogEnvelope.self, from: data)
    }

	static func decodeProfiles(_ data: Data) throws -> [HMuxProfile] {
		let envelope = try JSONDecoder.hmuxDecoder.decode(HMuxProfilesEnvelope.self, from: data)
		guard envelope.appProtocolVersion == appProtocolVersion else { throw HMuxBackendError.invalidProtocol }
		guard envelope.ok else {
			throw HMuxBackendError.backend(envelope.error?.message ?? "HMux profiles are unavailable.")
		}
		guard envelope.error == nil, let profiles = envelope.data?.profiles, !profiles.isEmpty else {
			throw HMuxBackendError.invalidProtocol
		}
		var profileIDs = Set<String>()
		for profile in profiles {
			guard hmuxIsValidProfileID(profile.id),
			      hmuxIsSafeMetadata(profile.label, maximumBytes: 256),
			      profile.tags.count <= 64,
			      profile.tags.allSatisfy({ hmuxIsSafeMetadata($0, maximumBytes: 64) }),
			      profileIDs.insert(profile.id).inserted else {
				throw HMuxBackendError.invalidProtocol
			}
		}
		return profiles
	}

	static func encodeCreateRequest(profileID: String, name: String) throws -> Data {
		guard hmuxIsValidProfileID(profileID), name.isEmpty || hmuxIsValidSessionName(name) else {
			throw HMuxBackendError.invalidProtocol
		}
		let encoder = JSONEncoder()
		encoder.keyEncodingStrategy = .convertToSnakeCase
		let data = try encoder.encode(HMuxCreateRequest(profileID: profileID, name: name.isEmpty ? nil : name))
		guard data.count <= 64 * 1024 else { throw HMuxBackendError.oversizedResponse }
		return data
	}

	static func decodeCreateResult(_ data: Data) throws -> HMuxSessionCreation {
		let envelope = try JSONDecoder.hmuxDecoder.decode(HMuxCreateEnvelope.self, from: data)
		guard envelope.appProtocolVersion == appProtocolVersion else { throw HMuxBackendError.invalidProtocol }
		guard envelope.ok else {
			throw HMuxBackendError.backend(envelope.error?.message ?? "HMux could not create the session.")
		}
		guard envelope.error == nil, let result = envelope.data,
		      hmuxIsValidSessionIdentity(result.session) else {
			throw HMuxBackendError.invalidProtocol
		}
		return result
	}

	static func decodeFileStageResult(
		_ data: Data,
		expectedRequestID: String,
		expectedSession: HMuxSessionIdentity,
		expectedExtensions: [String?]
	) throws -> HMuxFileStageResult {
		let envelope = try decodeFileStageEnvelope(data)
		guard envelope.appProtocolVersion == appProtocolVersion,
		      envelope.ok,
		      envelope.error == nil,
		      let result = envelope.data else {
			throw HMuxBackendError.invalidFileStage
		}
		try validateFileStageResult(
			result,
			expectedRequestID: expectedRequestID,
			expectedSession: expectedSession,
			expectedExtensions: expectedExtensions
		)
		return result
	}

	private static func decodeFileStageEnvelope(_ data: Data) throws -> HMuxFileStageEnvelope {
		guard !data.isEmpty, data.count <= maximumFileStageResponseBytes else {
			throw HMuxBackendError.oversizedResponse
		}
		try validateFileStageJSONShape(data)
		return try JSONDecoder().decode(HMuxFileStageEnvelope.self, from: data)
	}

	private static func validateFileStageJSONShape(_ data: Data) throws {
		guard let root = try JSONSerialization.jsonObject(with: data) as? [String: Any],
		      Set(root.keys).isSubset(of: ["app_protocol_version", "ok", "data", "error"]) else {
			throw HMuxBackendError.invalidFileStage
		}
		if let error = root["error"] as? [String: Any],
		   !Set(error.keys).isSubset(of: ["code", "message"]) {
			throw HMuxBackendError.invalidFileStage
		}
		guard let dataObject = root["data"] else { return }
		guard let result = dataObject as? [String: Any],
		      Set(result.keys) == ["protocol_version", "request_id", "stage_id", "session", "expires_at_unix", "files"],
		      let session = result["session"] as? [String: Any],
		      Set(session.keys) == ["id", "created_at"],
		      let files = result["files"] as? [[String: Any]],
		      files.allSatisfy({ Set($0.keys) == ["index", "path", "size", "sha256"] }) else {
			throw HMuxBackendError.invalidFileStage
		}
	}

	private static func validateFileStageResult(
		_ result: HMuxFileStageResult,
		expectedRequestID: String,
		expectedSession: HMuxSessionIdentity,
		expectedExtensions: [String?]
	) throws {
		guard result.protocolVersion == 1,
		      result.requestID == expectedRequestID,
		      result.session == expectedSession,
		      hmuxIsLowercaseHex(result.requestID, count: 32),
		      hmuxIsLowercaseHex(result.stageID, count: 32),
		      result.expiresAtUnix > 0,
		      result.files.count == expectedExtensions.count else {
			throw HMuxBackendError.invalidFileStage
		}
		let stageDirectory = "\(result.expiresAtUnix)-\(result.stageID)"
		for (index, file) in result.files.enumerated() {
			guard file.index == index,
			      (1...(32 * 1024 * 1024)).contains(file.size),
			      hmuxIsLowercaseHex(file.sha256, count: 64),
			      hmuxValidStagedPath(
					file.path,
					stageDirectory: stageDirectory,
					index: index,
					extension: expectedExtensions[index]
			      ) else {
				throw HMuxBackendError.invalidFileStage
			}
		}
	}

#if DEBUG
    private static func readFixture(at path: String) throws -> Data {
        guard path.hasPrefix("/") else { throw HMuxBackendError.commandFailed }
        let url = URL(fileURLWithPath: path)
        guard let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey]),
              values.isRegularFile == true,
              values.isSymbolicLink != true,
              let size = values.fileSize,
              size > 0,
              size <= maximumResponseBytes else { throw HMuxBackendError.oversizedResponse }
        return try Data(contentsOf: url, options: [.mappedIfSafe])
    }
#endif
}

private struct HMuxAliasRequest: Encodable {
    let session: HMuxSessionIdentity
    let alias: String
}

private struct HMuxCreateRequest: Encodable {
	let profileID: String
	let name: String?
}

private struct HMuxHiddenRequest: Encodable {
    let session: HMuxSessionIdentity
    let hidden: Bool
}

private struct HMuxTerminateRequest: Encodable {
    let session: HMuxSessionIdentity
    let confirmed: Bool
}

private struct HMuxFileStageRequest: Encodable {
	let requestID: String
	let session: HMuxSessionIdentity
	let paths: [String]
}

struct HMuxFileStageFile: Decodable, Equatable, Sendable {
	let index: Int
	let path: String
	let size: Int
	let sha256: String
}

struct HMuxFileStageResult: Decodable, Equatable, Sendable {
	let protocolVersion: Int
	let requestID: String
	let stageID: String
	let session: HMuxSessionIdentity
	let expiresAtUnix: Int64
	let files: [HMuxFileStageFile]

	private enum CodingKeys: String, CodingKey {
		case protocolVersion = "protocol_version"
		case requestID = "request_id"
		case stageID = "stage_id"
		case session
		case expiresAtUnix = "expires_at_unix"
		case files
	}

	private struct SessionPayload: Decodable {
		let id: String
		let createdAt: Int64

		private enum CodingKeys: String, CodingKey {
			case id
			case createdAt = "created_at"
		}
	}

	init(from decoder: Decoder) throws {
		let container = try decoder.container(keyedBy: CodingKeys.self)
		protocolVersion = try container.decode(Int.self, forKey: .protocolVersion)
		requestID = try container.decode(String.self, forKey: .requestID)
		stageID = try container.decode(String.self, forKey: .stageID)
		let payload = try container.decode(SessionPayload.self, forKey: .session)
		session = HMuxSessionIdentity(id: payload.id, createdAt: payload.createdAt)
		expiresAtUnix = try container.decode(Int64.self, forKey: .expiresAtUnix)
		files = try container.decode([HMuxFileStageFile].self, forKey: .files)
	}
}

private struct HMuxFileStageEnvelope: Decodable {
	let appProtocolVersion: Int
	let ok: Bool
	let data: HMuxFileStageResult?
	let error: HMuxAppErrorPayload?

	private enum CodingKeys: String, CodingKey {
		case appProtocolVersion = "app_protocol_version"
		case ok, data, error
	}
}

private struct HMuxMutationEnvelope: Decodable {
    let appProtocolVersion: Int
    let ok: Bool
    let error: HMuxAppErrorPayload?
}

private struct HMuxProfilesData: Decodable {
	let profiles: [HMuxProfile]
}

private struct HMuxProfilesEnvelope: Decodable {
	let appProtocolVersion: Int
	let ok: Bool
	let data: HMuxProfilesData?
	let error: HMuxAppErrorPayload?
}

private struct HMuxCreateEnvelope: Decodable {
	let appProtocolVersion: Int
	let ok: Bool
	let data: HMuxSessionCreation?
	let error: HMuxAppErrorPayload?
}

private struct HMuxNativeUpdateData: Decodable {
    let appUpdate: HMuxAppUpdate?
}

private struct HMuxNativeUpdateEnvelope: Decodable {
    let appProtocolVersion: Int
    let ok: Bool
    let data: HMuxNativeUpdateData?
    let error: HMuxAppErrorPayload?
}

private extension JSONDecoder {
    static var hmuxDecoder: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }
}

func hmuxShouldAutoPasteFileStage(
	targetMatches: Bool,
	tabSelected: Bool,
	epochMatches: Bool,
	appActive: Bool,
	keyWindow: Bool,
	hasSurfaceModel: Bool
) -> Bool {
	targetMatches && tabSelected && epochMatches && appActive && keyWindow && hasSurfaceModel
}

func hmuxCanPasteReadyFileStage(
	targetMatches: Bool,
	tabSelected: Bool,
	appActive: Bool,
	keyWindow: Bool,
	hasSurfaceModel: Bool
) -> Bool {
	targetMatches && tabSelected && appActive && keyWindow && hasSurfaceModel
}

func hmuxFileStageRequestID(_ operationID: UUID) -> String {
	operationID.uuidString.replacingOccurrences(of: "-", with: "").lowercased()
}

func hmuxFileStagePasteText(_ result: HMuxFileStageResult) throws -> String {
	let stageDirectory = "\(result.expiresAtUnix)-\(result.stageID)"
	guard hmuxIsLowercaseHex(result.stageID, count: 32), !result.files.isEmpty else {
		throw HMuxBackendError.invalidFileStage
	}
	var quoted: [String] = []
	for (index, file) in result.files.enumerated() {
		guard file.index == index,
		      hmuxValidStagedPath(
				file.path,
				stageDirectory: stageDirectory,
				index: index,
				extension: hmuxStagedPathExtension(file.path)
		      ) else {
			throw HMuxBackendError.invalidFileStage
		}
		quoted.append(try hmuxPOSIXQuote(file.path))
	}
	let text = quoted.joined(separator: " ")
	guard text.utf8.count <= 64 * 1024, !hmuxContainsControlCharacter(text) else {
		throw HMuxBackendError.invalidFileStage
	}
	return text
}

func hmuxPOSIXQuote(_ value: String) throws -> String {
	guard !value.isEmpty,
	      value.utf8.count <= 4096,
	      !hmuxContainsControlCharacter(value) else {
		throw HMuxBackendError.invalidFileStage
	}
	return "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
}

private func hmuxSafeFileStageExtension(_ path: String) -> String? {
	let value = URL(fileURLWithPath: path).pathExtension.lowercased()
	guard (1...16).contains(value.utf8.count),
	      value.unicodeScalars.allSatisfy({ scalar in
		      scalar.isASCII && ((scalar.value >= 48 && scalar.value <= 57) || (scalar.value >= 97 && scalar.value <= 122))
	      }) else { return nil }
	return value
}

private func hmuxStagedPathExtension(_ path: String) -> String? {
	let value = URL(fileURLWithPath: path).pathExtension
	return value.isEmpty ? nil : value
}

private func hmuxValidStagedPath(
	_ path: String,
	stageDirectory: String,
	index: Int,
	extension fileExtension: String?
) -> Bool {
	guard path.hasPrefix("/"),
	      path.utf8.count <= 4096,
	      !hmuxContainsControlCharacter(path),
	      (path as NSString).standardizingPath == path,
	      hmuxIsStageDirectory(stageDirectory) else { return false }
	let components = (path as NSString).pathComponents
	guard components.count >= 5,
	      components[components.count - 2] == stageDirectory,
	      components[components.count - 3] == "staged-files-v1",
	      components[components.count - 4] == "hmux" else { return false }
	var expectedName = String(format: "file-%04d", index + 1)
	if let fileExtension {
		guard hmuxSafeFileStageExtension("/x.\(fileExtension)") == fileExtension else { return false }
		expectedName += ".\(fileExtension)"
	}
	return components.last == expectedName
}

private func hmuxIsStageDirectory(_ value: String) -> Bool {
	guard value.utf8.count == 43, value[value.index(value.startIndex, offsetBy: 10)] == "-" else { return false }
	let timestamp = value.prefix(10)
	let stageID = value.dropFirst(11)
	return timestamp.allSatisfy(\.isNumber) && hmuxIsLowercaseHex(String(stageID), count: 32)
}

private func hmuxIsLowercaseHex(_ value: String, count: Int) -> Bool {
	value.utf8.count == count && value.utf8.allSatisfy { byte in
		(byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
	}
}

private func hmuxContainsControlCharacter(_ value: String) -> Bool {
	value.unicodeScalars.contains { scalar in
		scalar.value < 0x20 || scalar.value == 0x7f
	}
}

private func hmuxIsSafeMetadata(_ value: String, maximumBytes: Int) -> Bool {
	guard !value.isEmpty, value.utf8.count <= maximumBytes else { return false }
	return value.unicodeScalars.allSatisfy { scalar in
		!CharacterSet.controlCharacters.contains(scalar) &&
			scalar.value != 0x061c && scalar.value != 0x200e && scalar.value != 0x200f &&
			!(0x202a...0x202e).contains(scalar.value) &&
			!(0x2066...0x2069).contains(scalar.value)
	}
}

private struct HMuxFileStageExit: Sendable {
	let exitedNormally: Bool
	let status: Int32
}

private typealias HMuxProcessExit = HMuxFileStageExit

private final class HMuxBackendProcessRuntime: @unchecked Sendable {
	private let process: Process
	private let input: FileHandle
	let output: FileHandle
	private let stateLock = NSLock()
	private var exitTask: Task<HMuxProcessExit, Never>?
	private var shutdownTask: Task<Void, Never>?
	private var timeoutObserved = false

	private init(process: Process, input: FileHandle, output: FileHandle) {
		self.process = process
		self.input = input
		self.output = output
	}

	static func start(arguments: [String]) throws -> HMuxBackendProcessRuntime {
		let executable = try HMuxBackend.backendExecutable()
		let process = Process()
		process.executableURL = executable
		process.arguments = ["--no-update-check"] + arguments
		process.environment = HMuxBackend.backendEnvironment()
		let inputPipe = Pipe()
		let outputPipe = Pipe()
		process.standardInput = inputPipe
		process.standardOutput = outputPipe
		process.standardError = FileHandle.nullDevice
		try process.run()
		return HMuxBackendProcessRuntime(
			process: process,
			input: inputPipe.fileHandleForWriting,
			output: outputPipe.fileHandleForReading
		)
	}

	var didTimeOut: Bool { stateLock.withLock { timeoutObserved } }

	func send(_ data: Data) async throws {
		try await Task.detached(priority: .userInitiated) { [self] in
			try input.write(contentsOf: data)
			try input.close()
		}.value
	}

	func closeInput() {
		try? input.close()
	}

	func requestStop(timedOut: Bool) {
		stateLock.withLock {
			if timedOut { timeoutObserved = true }
			guard shutdownTask == nil else { return }
			try? input.close()
			try? output.close()
			if process.isRunning { process.terminate() }
			let exitTask = exitTaskLocked()
			let process = process
			shutdownTask = Task.detached(priority: .utility) {
				let deadline = Date().addingTimeInterval(2)
				while process.isRunning, Date() < deadline { usleep(20_000) }
				if process.isRunning { _ = Darwin.kill(process.processIdentifier, SIGKILL) }
				_ = await exitTask.value
			}
		}
	}

	func waitForExit() async -> HMuxProcessExit {
		let task = stateLock.withLock { exitTaskLocked() }
		return await task.value
	}

	private func exitTaskLocked() -> Task<HMuxProcessExit, Never> {
		if let exitTask { return exitTask }
		let process = process
		let task = Task.detached(priority: .utility) {
			process.waitUntilExit()
			return HMuxProcessExit(
				exitedNormally: process.terminationReason == .exit,
				status: process.terminationStatus
			)
		}
		exitTask = task
		return task
	}

	func shutdown() async {
		requestStop(timedOut: false)
		let task = stateLock.withLock { shutdownTask }
		await task?.value
	}
}

private final class HMuxFileStageProcessRuntime: @unchecked Sendable {
	private let process: Process
	private let input: FileHandle
	let output: FileHandle
	private let shutdownLock = NSLock()
	private var exitTask: Task<HMuxFileStageExit, Never>?
	private var shutdownTask: Task<Void, Never>?

	private init(process: Process, input: FileHandle, output: FileHandle) {
		self.process = process
		self.input = input
		self.output = output
	}

	static func start() throws -> HMuxFileStageProcessRuntime {
		let executable = try HMuxBackend.backendExecutable()
		let process = Process()
		process.executableURL = executable
		process.arguments = ["--no-update-check", "app", "file-stage"]
		var environment = HMuxBackend.backendEnvironment()
		environment["HMUX_FILE_STAGE_PARENT_PID"] = String(ProcessInfo.processInfo.processIdentifier)
		process.environment = environment
		let inputPipe = Pipe()
		let outputPipe = Pipe()
		process.standardInput = inputPipe
		process.standardOutput = outputPipe
		process.standardError = FileHandle.nullDevice
		try process.run()
		return HMuxFileStageProcessRuntime(
			process: process,
			input: inputPipe.fileHandleForWriting,
			output: outputPipe.fileHandleForReading
		)
	}

	func sendRequest(_ requestData: Data) async throws {
		try await Task.detached(priority: .userInitiated) { [self] in
			try input.write(contentsOf: requestData)
			try input.close()
		}.value
	}

	func requestStop() {
		shutdownLock.withLock {
			guard shutdownTask == nil else { return }
			try? input.close()
			try? output.close()
			if process.isRunning { process.terminate() }
			let exitTask = exitTaskLocked()
			let process = process
			shutdownTask = Task.detached(priority: .utility) {
				let deadline = Date().addingTimeInterval(2)
				while process.isRunning, Date() < deadline { usleep(20_000) }
				if process.isRunning { _ = Darwin.kill(process.processIdentifier, SIGKILL) }
				_ = await exitTask.value
			}
		}
	}

	func waitForExit() async -> HMuxFileStageExit {
		let task = shutdownLock.withLock { exitTaskLocked() }
		return await task.value
	}

	private func exitTaskLocked() -> Task<HMuxFileStageExit, Never> {
		if let exitTask { return exitTask }
		let process = process
		let task = Task.detached(priority: .utility) {
			process.waitUntilExit()
			return HMuxFileStageExit(
				exitedNormally: process.terminationReason == .exit,
				status: process.terminationStatus
			)
		}
		exitTask = task
		return task
	}

	func shutdown() async {
		requestStop()
		let task = shutdownLock.withLock { shutdownTask }
		await task?.value
	}
}

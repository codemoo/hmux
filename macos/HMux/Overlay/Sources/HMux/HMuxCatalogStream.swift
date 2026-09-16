import CryptoKit
import Darwin
import Foundation
import Security

enum HMuxCatalogTransportMode: Equatable {
    case webSocket
    case pollingFallback
}

enum HMuxCatalogConnectionState: Equatable {
    case starting
    case awaitingInitialSnapshot
    case connected(HMuxCatalogTransportMode)
    case reconnecting
    case offline
}

private struct HMuxCatalogStreamBootstrap: Decodable {
    let appProtocolVersion: Int
    let streamProtocolVersion: Int
    let supported: Bool
    let url: String?
    let token: String?
    let certificateSha256: String?
    let expiresAt: String?
    let maximumFrameBytes: Int
	let workspaceSourceKey: String?
}

private struct HMuxCatalogStreamMessage: Decodable {
    let appProtocolVersion: Int
    let streamProtocolVersion: Int
    let streamId: String
    let sequence: UInt64
    let type: String
    let data: HMuxCatalog
}

final class HMuxCatalogStreamConnection: @unchecked Sendable {
    private static let bootstrapLimit = 64 * 1024
    private static let bootstrapTimeout: TimeInterval = 10
    private static let maximumFrameBytes = 32 * 1024 * 1024

    private let process: Process
    private let session: URLSession
    private let webSocket: URLSessionWebSocketTask
    private let delegate: HMuxPinnedSessionDelegate
	private let workspaceSourceKey: String?
    private var streamID: String?
    private var nextSequence: UInt64 = 1
    private var isClosed = false
	private let closeLock = NSLock()

    private init(
        process: Process,
        session: URLSession,
        webSocket: URLSessionWebSocketTask,
        delegate: HMuxPinnedSessionDelegate,
		workspaceSourceKey: String?
    ) {
        self.process = process
        self.session = session
        self.webSocket = webSocket
        self.delegate = delegate
		self.workspaceSourceKey = workspaceSourceKey
    }

	static func open() async throws -> HMuxCatalogStreamConnection {
		let openingTask = Task.detached(priority: .utility) {
			try openSynchronously()
		}
		return try await withTaskCancellationHandler {
			let connection = try await openingTask.value
			do {
				try Task.checkCancellation()
				return connection
			} catch {
				await connection.close()
				throw error
			}
		} onCancel: {
			openingTask.cancel()
		}
	}

	private static func openSynchronously() throws -> HMuxCatalogStreamConnection {
        let executable = try HMuxBackend.backendExecutable()
        let process = Process()
        process.executableURL = executable
		process.arguments = ["--no-update-check", "app", "catalog-stream"]
		var environment = HMuxBackend.backendEnvironment()
		environment["HMUX_CATALOG_PARENT_PID"] = String(getpid())
		process.environment = environment
        let stdout = Pipe()
        process.standardOutput = stdout
        process.standardError = FileHandle.nullDevice
        try process.run()

        let bootstrapData: Data
        do {
            bootstrapData = try readBootstrap(from: stdout.fileHandleForReading)
        } catch {
            stop(process)
            throw error
        }
        let decoder = JSONDecoder()
		decoder.keyDecodingStrategy = .convertFromSnakeCase
        let bootstrap: HMuxCatalogStreamBootstrap
        do {
            bootstrap = try decoder.decode(HMuxCatalogStreamBootstrap.self, from: bootstrapData)
        } catch {
			debugFailure("bootstrap decode failed: \(error)")
            stop(process)
            throw HMuxBackendError.invalidStreamBootstrap
        }
        guard bootstrap.appProtocolVersion == HMuxBackend.appProtocolVersion,
              bootstrap.streamProtocolVersion == 1,
              bootstrap.maximumFrameBytes == maximumFrameBytes else {
			debugFailure("bootstrap protocol or frame limit mismatch")
            stop(process)
            throw HMuxBackendError.invalidStreamBootstrap
        }
        guard bootstrap.supported else {
            stop(process)
            throw HMuxBackendError.streamUnsupported
        }
		if let key = bootstrap.workspaceSourceKey, !hmuxIsValidWorkspaceSourceKey(key) {
			stop(process)
			throw HMuxBackendError.invalidStreamBootstrap
		}
        guard let rawURL = bootstrap.url,
              let url = URL(string: rawURL),
              url.scheme == "wss",
              url.host == "127.0.0.1",
              url.path == "/catalog",
              url.query == nil,
              url.fragment == nil,
              url.user == nil,
              url.password == nil,
              let port = url.port,
              (1...65535).contains(port),
              let token = bootstrap.token,
              decodeBase64URL(token)?.count == 32,
              let pinText = bootstrap.certificateSha256,
              let pin = decodeHex(pinText),
              pin.count == SHA256.byteCount,
              let expirationText = bootstrap.expiresAt,
              let expiration = parseISO8601(expirationText),
              expiration > Date(),
              expiration.timeIntervalSinceNow <= 6 * 60 else {
			debugFailure("bootstrap endpoint, credential, pin, or expiry validation failed")
            stop(process)
            throw HMuxBackendError.invalidStreamBootstrap
        }

        let delegate = HMuxPinnedSessionDelegate(expectedPin: pin, expectedPort: port)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.urlCache = nil
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.timeoutIntervalForRequest = 10
        configuration.timeoutIntervalForResource = 0
        configuration.waitsForConnectivity = false
        let delegateQueue = OperationQueue()
        delegateQueue.maxConcurrentOperationCount = 1
        delegateQueue.qualityOfService = .utility
        let session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: delegateQueue)
        var request = URLRequest(url: url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 10
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        let task = session.webSocketTask(with: request)
        task.maximumMessageSize = maximumFrameBytes
        let connection = HMuxCatalogStreamConnection(
            process: process,
            session: session,
            webSocket: task,
            delegate: delegate,
			workspaceSourceKey: bootstrap.workspaceSourceKey
        )
        task.resume()
        return connection
    }

    func receiveCatalog() async throws -> HMuxCatalog {
		let operation = HMuxWebSocketReceiveOperation(webSocket: webSocket)
		let message = try await withTaskCancellationHandler {
			try await operation.receive()
		} onCancel: {
			operation.cancel()
		}
        let data: Data
        switch message {
        case .data(let value): data = value
        case .string(let value):
            guard let encoded = value.data(using: .utf8) else { throw HMuxBackendError.invalidProtocol }
            data = encoded
        @unknown default:
            throw HMuxBackendError.invalidProtocol
        }
        guard !data.isEmpty, data.count <= Self.maximumFrameBytes else {
            throw HMuxBackendError.oversizedResponse
        }
        let decoder = JSONDecoder()
		decoder.keyDecodingStrategy = .convertFromSnakeCase
        let envelope = try decoder.decode(HMuxCatalogStreamMessage.self, from: data)
        guard envelope.appProtocolVersion == HMuxBackend.appProtocolVersion,
              envelope.streamProtocolVersion == 1,
              envelope.type == "snapshot",
              envelope.data.protocolVersion == HMuxBackend.backendProtocolVersion,
              !envelope.streamId.isEmpty,
              envelope.streamId.count <= 128,
              envelope.streamId.unicodeScalars.allSatisfy({
                  CharacterSet.alphanumerics.contains($0) || $0 == "-" || $0 == "_"
              }) else {
            throw HMuxBackendError.invalidProtocol
        }
        if let streamID, streamID != envelope.streamId {
            throw HMuxBackendError.streamSequenceGap
        }
        guard envelope.sequence == nextSequence else {
            throw HMuxBackendError.streamSequenceGap
        }
        streamID = envelope.streamId
        nextSequence += 1
		var catalog = envelope.data
		catalog.workspaceSourceKey = workspaceSourceKey
		return catalog
    }

    func close() async {
		guard beginClose() else { return }
        webSocket.cancel(with: .goingAway, reason: nil)
        session.invalidateAndCancel()
        let process = self.process
        await Task.detached(priority: .utility) {
            Self.stop(process)
        }.value
    }

	private func beginClose() -> Bool {
		closeLock.lock()
		defer { closeLock.unlock() }
		guard !isClosed else { return false }
		isClosed = true
		return true
	}

    deinit {
        if process.isRunning { process.terminate() }
    }

    private static func readBootstrap(from handle: FileHandle) throws -> Data {
        defer { try? handle.close() }
        var result = Data()
        var foundNewline = false
		let deadline = Date().addingTimeInterval(bootstrapTimeout)
        while result.count <= bootstrapLimit {
			try Task.checkCancellation()
			guard Date() < deadline else { throw HMuxBackendError.invalidStreamBootstrap }
			var descriptor = pollfd(
				fd: handle.fileDescriptor,
				events: Int16(POLLIN | POLLHUP | POLLERR),
				revents: 0
			)
			let pollResult = Darwin.poll(&descriptor, 1, 100)
			if pollResult < 0 {
				if errno == EINTR { continue }
				throw HMuxBackendError.invalidStreamBootstrap
			}
			if pollResult == 0 { continue }
			guard descriptor.revents & Int16(POLLNVAL) == 0 else {
				throw HMuxBackendError.invalidStreamBootstrap
			}
            let chunk = handle.readData(ofLength: 4096)
            if chunk.isEmpty { break }
            result.append(chunk)
            if let newline = result.firstIndex(of: 0x0A) {
                let trailing = result[result.index(after: newline)...]
                guard trailing.allSatisfy({ byte in byte == 0x20 || byte == 0x09 || byte == 0x0D || byte == 0x0A }) else {
                    throw HMuxBackendError.invalidStreamBootstrap
                }
                result = result[..<newline]
                foundNewline = true
                break
            }
        }
        guard foundNewline, !result.isEmpty, result.count <= bootstrapLimit else {
            throw HMuxBackendError.invalidStreamBootstrap
        }
        return result
    }

    private static func stop(_ process: Process) {
        guard process.isRunning else {
            process.waitUntilExit()
            return
        }
        process.terminate()
        let deadline = Date().addingTimeInterval(2)
        while process.isRunning, Date() < deadline {
            usleep(20_000)
        }
        if process.isRunning {
            _ = Darwin.kill(process.processIdentifier, SIGKILL)
        }
        process.waitUntilExit()
    }

    private static func decodeHex(_ value: String) -> Data? {
        guard value.count == SHA256.byteCount * 2 else { return nil }
        var result = Data()
        result.reserveCapacity(SHA256.byteCount)
        var index = value.startIndex
        for _ in 0..<SHA256.byteCount {
            let next = value.index(index, offsetBy: 2)
            guard let byte = UInt8(value[index..<next], radix: 16) else { return nil }
            result.append(byte)
            index = next
        }
        return result
    }

    private static func decodeBase64URL(_ value: String) -> Data? {
        guard value.unicodeScalars.allSatisfy({
            CharacterSet.alphanumerics.contains($0) || $0 == "-" || $0 == "_"
        }) else { return nil }
        var padded = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        padded += String(repeating: "=", count: (4 - padded.count % 4) % 4)
        return Data(base64Encoded: padded)
    }

	private static func parseISO8601(_ value: String) -> Date? {
		let fractional = ISO8601DateFormatter()
		fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
		if let date = fractional.date(from: value) { return date }
		return ISO8601DateFormatter().date(from: value)
	}

	private static func debugFailure(_ message: String) {
#if DEBUG
		FileHandle.standardError.write(Data(("HMux stream debug: " + message + "\n").utf8))
#endif
	}
}

func hmuxMergedAppUpdate(
	current: HMuxAppUpdate?,
	incoming: HMuxAppUpdate?,
	mode: HMuxCatalogTransportMode
) -> HMuxAppUpdate? {
	mode == .webSocket ? current : incoming
}

func hmuxShouldAcceptCatalogResult(
	generation: UInt64,
	currentGeneration: UInt64,
	activeWebSocketGeneration: UInt64?,
	mode: HMuxCatalogTransportMode,
	startingRevision: UInt64,
	currentRevision: UInt64
) -> Bool {
	guard generation == currentGeneration else { return false }
	if mode == .webSocket { return true }
	return activeWebSocketGeneration != generation && startingRevision == currentRevision
}

func hmuxNextCatalogStreamFailureCount(
	previous: Int,
	connectionLifetime: TimeInterval,
	stableWindow: TimeInterval = 120
) -> Int {
	guard stableWindow > 0, connectionLifetime >= stableWindow else {
		return previous < Int.max ? previous + 1 : Int.max
	}
	return 1
}

func hmuxCatalogOutageShouldBeOffline(
	hasReceivedCatalog: Bool,
	disconnectedAt: Date,
	now: Date,
	gracePeriod: TimeInterval = 20
) -> Bool {
	!hasReceivedCatalog || gracePeriod <= 0 || now.timeIntervalSince(disconnectedAt) >= gracePeriod
}

private final class HMuxWebSocketReceiveOperation: @unchecked Sendable {
	typealias Message = URLSessionWebSocketTask.Message

	private static let pingInterval: TimeInterval = 10
	private static let pingTimeout: TimeInterval = 5
	private static let watchdogQueue = DispatchQueue(
		label: "dev.hmux.catalog-stream-watchdog",
		qos: .utility
	)

	private let webSocket: URLSessionWebSocketTask
	private let lock = NSLock()
	private var continuation: CheckedContinuation<Message, Error>?
	private var pendingResult: Result<Message, Error>?
	private var started = false
	private var completed = false
	private var pingWorkItem: DispatchWorkItem?
	private var pingTimeoutWorkItem: DispatchWorkItem?

	init(webSocket: URLSessionWebSocketTask) {
		self.webSocket = webSocket
	}

	func receive() async throws -> Message {
		try await withCheckedThrowingContinuation { continuation in
			lock.lock()
			if let pendingResult {
				self.pendingResult = nil
				lock.unlock()
				continuation.resume(with: pendingResult)
				return
			}
			self.continuation = continuation
			let shouldStart = !started
			started = true
			lock.unlock()
			if shouldStart { start() }
		}
	}

	func cancel() {
		if finish(.failure(CancellationError())) {
			webSocket.cancel(with: .goingAway, reason: nil)
		}
	}

	private func start() {
		webSocket.receive { [weak self] result in
			_ = self?.finish(result)
		}
		armPing()
	}

	private func armPing() {
		let workItem = DispatchWorkItem { [weak self] in self?.sendPing() }
		lock.lock()
		guard !completed else {
			lock.unlock()
			return
		}
		pingWorkItem?.cancel()
		pingWorkItem = workItem
		lock.unlock()
		Self.watchdogQueue.asyncAfter(deadline: .now() + Self.pingInterval, execute: workItem)
	}

	private func sendPing() {
		let timeout = DispatchWorkItem { [weak self] in
			_ = self?.finish(.failure(HMuxBackendError.streamLivenessTimeout))
		}
		lock.lock()
		guard !completed else {
			lock.unlock()
			return
		}
		pingTimeoutWorkItem?.cancel()
		pingTimeoutWorkItem = timeout
		lock.unlock()
		Self.watchdogQueue.asyncAfter(deadline: .now() + Self.pingTimeout, execute: timeout)
		webSocket.sendPing { [weak self] error in
			guard let self else { return }
			if let error {
				_ = self.finish(.failure(error))
				return
			}
			self.lock.lock()
			guard !self.completed else {
				self.lock.unlock()
				return
			}
			self.pingTimeoutWorkItem?.cancel()
			self.pingTimeoutWorkItem = nil
			self.lock.unlock()
			self.armPing()
		}
	}

	@discardableResult
	private func finish(_ result: Result<Message, Error>) -> Bool {
		lock.lock()
		guard !completed else {
			lock.unlock()
			return false
		}
		completed = true
		pingWorkItem?.cancel()
		pingTimeoutWorkItem?.cancel()
		pingWorkItem = nil
		pingTimeoutWorkItem = nil
		let continuation = self.continuation
		self.continuation = nil
		if continuation == nil { pendingResult = result }
		lock.unlock()
		continuation?.resume(with: result)
		return true
	}
}

private final class HMuxPinnedSessionDelegate: NSObject, URLSessionDelegate, @unchecked Sendable {
    private let expectedPin: Data
    private let expectedPort: Int

    init(expectedPin: Data, expectedPort: Int) {
        self.expectedPin = expectedPin
        self.expectedPort = expectedPort
    }

    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        let protectionSpace = challenge.protectionSpace
        guard protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              protectionSpace.host == "127.0.0.1",
              protectionSpace.port == expectedPort,
              let trust = protectionSpace.serverTrust,
			  let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
			  chain.count == 1,
			  let certificate = chain.first else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        let certificateData = SecCertificateCopyData(certificate) as Data
        let digest = Data(SHA256.hash(data: certificateData))
        guard constantTimeEqual(digest, expectedPin) else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        completionHandler(.useCredential, URLCredential(trust: trust))
    }

    private func constantTimeEqual(_ left: Data, _ right: Data) -> Bool {
        guard left.count == right.count else { return false }
        var difference: UInt8 = 0
        for index in left.indices {
            difference |= left[index] ^ right[index]
        }
        return difference == 0
    }
}

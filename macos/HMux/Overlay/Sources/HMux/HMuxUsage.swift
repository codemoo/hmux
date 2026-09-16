import Combine
import Darwin
import Foundation
import os

enum HMuxUsageProvider: String, CaseIterable, Codable, Sendable {
    case claude
    case codex

    var label: String { self == .claude ? "Claude" : "Codex" }
}

struct HMuxUsageWindow: Codable, Equatable, Sendable {
    let usedPct: Double
    let remainingSeconds: Int
    let resetsAt: String?

    enum CodingKeys: String, CodingKey {
        case usedPct = "used_pct"
        case remainingSeconds = "remaining_seconds"
        case resetsAt = "resets_at"
    }
}

struct HMuxUsageAccountWindow: Codable, Equatable, Sendable {
    let usedPct: Double
    let resetsAt: String?

    enum CodingKeys: String, CodingKey {
        case usedPct = "used_pct"
        case resetsAt = "resets_at"
    }
}

struct HMuxUsageAccount: Codable, Equatable, Sendable, Identifiable {
    let number: Int
    let email: String
    let displayName: String?
    let active: Bool
    let status: String
    let fiveHour: HMuxUsageAccountWindow?
    let sevenDay: HMuxUsageAccountWindow?
    let tokensPerHour: Double?
    let totalTokens: Int64?
    let lastRefreshAt: String?

    var id: Int { number }
    var label: String { !email.isEmpty ? email : (displayName?.isEmpty == false ? displayName! : "Account \(number)") }

    enum CodingKeys: String, CodingKey {
        case number, email, active, status
        case displayName = "display_name"
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
        case tokensPerHour = "tokens_per_hour"
        case totalTokens = "total_tokens"
        case lastRefreshAt = "last_refresh_at"
    }
}

struct HMuxUsageSnapshotStatus: Codable, Equatable, Sendable {
    let state: String
    let dataSource: String?
    let quotaSource: String?
    let stale: Bool
    let quotaObservedAt: String?
	let retryAt: String?

	var retryDate: Date? { retryAt.flatMap(HMuxUsageTimestamp.date) }

    enum CodingKeys: String, CodingKey {
        case state
        case dataSource = "data_source"
        case quotaSource = "quota_source"
        case stale
        case quotaObservedAt = "quota_observed_at"
		case retryAt = "retry_at"
    }
}

struct HMuxUsageSnapshot: Codable, Equatable, Sendable {
    let schema: Int
    let seq: Int
    let generatedAtUTC: String
    let provider: HMuxUsageProvider
    let burnRatePerMinute: Double
    let burnState: String
    let todayTotalTokens: Int
    let todaySessions: Int
    let rolling5h: HMuxUsageWindow
    let weekly: HMuxUsageWindow
    let rolling5hObserved: Bool?
    let weeklyObserved: Bool?
    let status: HMuxUsageSnapshotStatus
    let accounts: [HMuxUsageAccount]?
    let accountsUpdatedAt: String?

    enum CodingKeys: String, CodingKey {
        case schema, seq, provider, weekly, status, accounts
        case generatedAtUTC = "generated_at_utc"
        case burnRatePerMinute = "burn_rate_per_min"
        case burnState = "burn_state"
        case todayTotalTokens = "today_total_tokens"
        case todaySessions = "today_sessions"
        case rolling5h = "rolling_5h"
        case rolling5hObserved = "rolling_5h_observed"
        case weeklyObserved = "weekly_observed"
        case accountsUpdatedAt = "accounts_updated_at"
    }

    func validated(for expectedProvider: HMuxUsageProvider) throws -> HMuxUsageSnapshot {
        guard schema == 1, seq >= 0, provider == expectedProvider,
              generatedAtUTC.utf8.count <= 128,
              burnState.utf8.count <= 32,
              burnRatePerMinute.isFinite, burnRatePerMinute >= 0,
              todayTotalTokens >= 0, todaySessions >= 0,
              Self.valid(window: rolling5h), Self.valid(window: weekly),
              status.state.utf8.count <= 64,
              (status.dataSource?.utf8.count ?? 0) <= 128,
			  (status.quotaSource?.utf8.count ?? 0) <= 128,
			  (status.quotaObservedAt?.utf8.count ?? 0) <= 128,
			  HMuxUsageTimestamp.validRetryAt(
				status.retryAt,
				generatedAt: generatedAtUTC,
				state: status.state,
				stale: status.stale
			  ),
			  (accountsUpdatedAt?.utf8.count ?? 0) <= 128,
              (accounts?.count ?? 0) <= 128
        else { throw HMuxUsageValidationError.invalidSnapshot }

        var accountNumbers = Set<Int>()
        for account in accounts ?? [] {
            guard account.number > 0, accountNumbers.insert(account.number).inserted,
                  (provider == .claude ? Self.valid(displayName: account.email) : account.email.isEmpty),
                  Self.valid(displayName: account.displayName),
                  (account.lastRefreshAt?.utf8.count ?? 0) <= 128,
                  account.status.utf8.count <= 64,
                  (account.tokensPerHour == nil ||
                    (account.tokensPerHour!.isFinite && account.tokensPerHour! >= 0)),
                  (account.totalTokens == nil || account.totalTokens! >= 0),
                  Self.valid(accountWindow: account.fiveHour),
                  Self.valid(accountWindow: account.sevenDay)
            else { throw HMuxUsageValidationError.invalidSnapshot }
        }
        return self
    }

    private static func valid(displayName: String?) -> Bool {
        guard let displayName else { return true }
        return displayName.utf8.count <= 256 && displayName.unicodeScalars.allSatisfy {
            !CharacterSet.controlCharacters.contains($0) &&
                !(0x202A...0x202E).contains($0.value) && !(0x2066...0x2069).contains($0.value)
        }
    }

    var isCodexPool: Bool { provider == .codex && status.quotaSource == "codex_lb" }

    var isClaudeSwap: Bool { provider == .claude && !(accounts ?? []).isEmpty }
    var activeClaudeAccount: HMuxUsageAccount? {
        guard isClaudeSwap else { return nil }
        let active = (accounts ?? []).filter(\.active)
        return active.count == 1 ? active[0] : nil
    }

    var quotaWindows: [HMuxUsageWindowDisplay] {
        if isClaudeSwap {
            let account = activeClaudeAccount
            let usable = account?.status == "ok" || account?.status == "stale"
            return [
                HMuxUsageWindowDisplay(label: "5h", usedPct: usable ? account?.fiveHour?.usedPct : nil, resetsAt: account?.fiveHour?.resetsAt),
                HMuxUsageWindowDisplay(label: "1w", usedPct: usable ? account?.sevenDay?.usedPct : nil, resetsAt: account?.sevenDay?.resetsAt),
            ]
        }
        return [
            window(label: "5h", value: rolling5h, observed: rolling5hObserved),
            window(label: "1w", value: weekly, observed: weeklyObserved),
        ]
    }

    private func window(label: String, value: HMuxUsageWindow, observed: Bool?) -> HMuxUsageWindowDisplay {
        // Older collectors did not distinguish a missing window from 0% used.
        // Accept their window only when there is evidence of a real observation.
        let known = observed == true ||
            (observed == nil && (value.usedPct > 0 || value.resetsAt != nil || value.remainingSeconds > 0))
        return HMuxUsageWindowDisplay(
            label: label, usedPct: known ? value.usedPct : nil,
            resetsAt: known ? value.resetsAt : nil
        )
    }

    private static func valid(window: HMuxUsageWindow) -> Bool {
        window.usedPct.isFinite && (0...1).contains(window.usedPct) &&
            window.remainingSeconds >= 0 && (window.resetsAt?.utf8.count ?? 0) <= 128
    }

    private static func valid(accountWindow: HMuxUsageAccountWindow?) -> Bool {
        guard let accountWindow else { return true }
        return accountWindow.usedPct.isFinite && (0...1).contains(accountWindow.usedPct) &&
            (accountWindow.resetsAt?.utf8.count ?? 0) <= 128
    }
}

enum HMuxUsageTimestamp {
	static func date(_ value: String) -> Date? {
		guard !value.isEmpty, value.utf8.count <= 128 else { return nil }
		let fractional = ISO8601DateFormatter()
		fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
		return fractional.date(from: value) ?? ISO8601DateFormatter().date(from: value)
	}

	static func validRetryAt(_ value: String?, generatedAt: String, state: String, stale: Bool) -> Bool {
		guard let value else { return true }
		guard state == "rateLimited" || (state == "ok" && stale),
			let retryAt = date(value), let generated = date(generatedAt)
		else { return false }
		let delay = retryAt.timeIntervalSince(generated)
		return delay > 0 && delay <= 24 * 60 * 60
	}
}

struct HMuxUsageWindowDisplay: Equatable {
    let label: String
    let usedPct: Double?
    let resetsAt: String?
}

private enum HMuxUsageValidationError: Error {
    case invalidSnapshot
}

enum HMuxUsagePhase: String, Equatable, Sendable {
    case connecting
    case connected
    case offline
	case updateRequired
}

struct HMuxProviderUsageState: Equatable, Sendable {
    var phase: HMuxUsagePhase = .connecting
    var snapshot: HMuxUsageSnapshot?
    var receivedAt: Date?
    var transportStale = false

    var isStale: Bool {
        if snapshot?.isClaudeSwap == true {
            return transportStale || snapshot?.activeClaudeAccount?.status == "stale" || phase != .connected
        }
        return transportStale || snapshot?.status.stale == true || (snapshot != nil && phase != .connected)
    }
	var hasUsableQuota: Bool {
        phase == .connected && (snapshot?.isClaudeSwap == true || snapshot?.status.state == "ok") &&
            snapshot?.quotaWindows.contains(where: { $0.label == "1w" && $0.usedPct != nil }) == true
    }
}

struct HMuxUsageSummary: Equatable, Sendable {
    let provider: HMuxUsageProvider
    let isAccountPool: Bool
    let remainingPercent: Int?
	let status: HMuxUsageSummaryStatus
    let stale: Bool
}

enum HMuxUsageSummaryStatus: Equatable, Sendable {
	case quota
	case starting
	case offline
	case homeSession
	case network
	case limited
	case changed
	case updateRequired
	case unavailable
}

struct HMuxUsageRestartBackoff: Equatable, Sendable {
	private(set) var nextDelay: UInt64 = 1

	mutating func consumeDelay() -> UInt64 {
		let delay = nextDelay
		nextDelay = min(60, nextDelay * 2)
		return delay
	}

	mutating func markStable() {
		nextDelay = 1
	}
}

enum HMuxBedlBurnState: Int, Comparable, Sendable {
    case idle, walk, jog, run, fly, rocket

    static func < (lhs: HMuxBedlBurnState, rhs: HMuxBedlBurnState) -> Bool {
        lhs.rawValue < rhs.rawValue
    }

    init(rawValueString: String) {
        switch rawValueString.lowercased() {
        case "walk": self = .walk
        case "jog": self = .jog
        case "run": self = .run
        case "fly": self = .fly
        case "rocket": self = .rocket
        default: self = .idle
        }
    }

    var cycleDuration: TimeInterval? {
        switch self {
        case .idle: return nil
        case .walk: return 2.25
        case .jog: return 1.50
        case .run: return 1.00
        case .fly: return 0.65
        case .rocket: return 0.50
        }
    }
}

@MainActor
final class HMuxTokenUsageStore: ObservableObject {
	private static let logger = Logger(subsystem: "dev.hmux.app", category: "usage")
    @Published private(set) var claude = HMuxProviderUsageState()
    @Published private(set) var codex = HMuxProviderUsageState()

	private var streamTask: Task<Void, Never>?
	private var staleTask: Task<Void, Never>?
	private var streamStabilityTask: Task<Void, Never>?
	private var streamGeneration: UInt64 = 0
	private var streamRestartBackoff = HMuxUsageRestartBackoff()
	private var shouldRun = false

    func start() {
		guard !shouldRun else { return }
		shouldRun = true
		Self.logger.info("starting Home usage stream")
		launchStream(after: 0)
		if staleTask == nil {
			staleTask = Task { [weak self] in
				while !Task.isCancelled {
					do { try await Task.sleep(nanoseconds: 15_000_000_000) }
					catch { return }
					self?.markSilentStreamsStale(now: Date())
				}
			}
		}
	}

	private func launchStream(after delay: UInt64) {
		streamGeneration &+= 1
		let generation = streamGeneration
		streamTask?.cancel()
		streamStabilityTask?.cancel()
		streamStabilityTask = nil
		streamTask = Task { [weak self] in
			if delay > 0 {
				do { try await Task.sleep(nanoseconds: delay * 1_000_000_000) }
				catch { return }
			}
			guard let self, self.shouldRun, generation == self.streamGeneration else { return }
			do {
				try await HMuxHomeUsageStream.run { [weak self] snapshot in
					await self?.receive(snapshot)
				}
			} catch {
				// Error details can include SSH metadata or Home paths. The UI needs
				// only the bounded offline state and automatic retry behavior.
			}
			guard !Task.isCancelled,
				self.shouldRun,
				generation == self.streamGeneration
			else { return }
			self.streamDidExit(generation: generation)
		}
	}

    func stop() {
		shouldRun = false
		streamGeneration &+= 1
		streamTask?.cancel()
		streamTask = nil
		streamStabilityTask?.cancel()
		streamStabilityTask = nil
        staleTask?.cancel()
        staleTask = nil
	}

	private func streamDidExit(generation: UInt64) {
		guard shouldRun, generation == streamGeneration else { return }
		Self.logger.error("Home usage stream exited; reconnecting")
		streamTask = nil
		streamStabilityTask?.cancel()
		streamStabilityTask = nil
		markAllOffline()
		launchStream(after: streamRestartBackoff.consumeDelay())
	}

	private func armStreamStabilityReset() {
		guard streamStabilityTask == nil else { return }
		let generation = streamGeneration
		streamStabilityTask = Task { [weak self] in
			do { try await Task.sleep(nanoseconds: 60_000_000_000) }
			catch { return }
			guard let self,
				self.shouldRun,
				generation == self.streamGeneration,
				self.streamTask != nil
			else { return }
			self.streamRestartBackoff.markStable()
			self.streamStabilityTask = nil
		}
	}

	private func markAllOffline() {
		for provider in HMuxUsageProvider.allCases {
			update(provider) {
				guard $0.phase != .updateRequired else { return }
				$0.phase = .offline
				if $0.snapshot != nil { $0.transportStale = true }
			}
		}
	}

    func state(for provider: HMuxUsageProvider) -> HMuxProviderUsageState {
        provider == .claude ? claude : codex
    }

    func summary(for provider: HMuxUsageProvider) -> HMuxUsageSummary {
        let state = state(for: provider)
		let remaining = state.hasUsableQuota
			? state.snapshot.flatMap { Self.remainingPercent(snapshot: $0) }
			: nil
        return HMuxUsageSummary(
            provider: provider,
            isAccountPool: state.snapshot?.isCodexPool == true,
			remainingPercent: remaining,
			status: Self.summaryStatus(state: state, hasQuota: remaining != nil),
            stale: state.isStale
        )
    }

	static func summaryStatus(state: HMuxProviderUsageState, hasQuota: Bool) -> HMuxUsageSummaryStatus {
		switch state.phase {
		case .connecting:
			return .starting
		case .offline:
			return .offline
		case .updateRequired:
			return .updateRequired
		case .connected:
            if hasQuota { return .quota }
            if state.snapshot?.isClaudeSwap == true { return .unavailable }
            switch state.snapshot?.status.state {
			case "authExpired", "codexLoggedOut": return .homeSession
			case "networkError": return .network
			case "rateLimited": return .limited
			case "quotaEndpointChanged": return .changed
			default: return .unavailable
			}
		}
	}

    var burnState: HMuxBedlBurnState {
        HMuxUsageProvider.allCases.reduce(.idle) { current, provider in
            let state = state(for: provider)
            guard !state.isStale, let snapshot = state.snapshot else { return current }
            return max(current, HMuxBedlBurnState(rawValueString: snapshot.burnState))
        }
    }

    static func remainingPercent(snapshot: HMuxUsageSnapshot) -> Int? {
        // Footer and headline consistently show the weekly window. The pool's
        // own capacity-weighted quota is authoritative; account percentages are
        // detail rows, not interchangeable capacities to average locally.
        guard let used = snapshot.quotaWindows.first(where: { $0.label == "1w" })?.usedPct else { return nil }
        return Int((100 * (1 - min(max(used, 0), 1))).rounded())
    }

	private func receive(_ event: HMuxUsageStreamEvent) {
		armStreamStabilityReset()
		switch event {
		case .heartbeat:
			break
		case .homeAgentUpdateRequired:
			for provider in HMuxUsageProvider.allCases {
				update(provider) { state in
					state.phase = .updateRequired
					if state.snapshot != nil { state.transportStale = true }
				}
			}
		case .snapshot(let snapshot):
			update(snapshot.provider) { state in
				state.phase = .connected
				state.snapshot = snapshot
				state.receivedAt = Date()
				state.transportStale = false
			}
		}
	}

    private func markSilentStreamsStale(now: Date) {
        for provider in HMuxUsageProvider.allCases {
            update(provider) { state in
                guard !state.transportStale, let receivedAt = state.receivedAt,
                      now.timeIntervalSince(receivedAt) >= 75 else { return }
                state.transportStale = true
            }
        }
    }

    private func update(_ provider: HMuxUsageProvider, _ mutation: (inout HMuxProviderUsageState) -> Void) {
        if provider == .claude {
            var value = claude
            mutation(&value)
            if value != claude { claude = value }
        } else {
            var value = codex
            mutation(&value)
            if value != codex { codex = value }
        }
    }
}

struct HMuxUsageStreamFrame: Decodable, Sendable {
	let protocolVersion: Int
	let sequence: UInt64
	let type: String
	let code: String?
	let provider: HMuxUsageProvider?
	let snapshot: HMuxUsageSnapshot?

	enum CodingKeys: String, CodingKey {
		case protocolVersion = "protocol_version"
		case sequence, type, code, provider, snapshot
	}

	func validated(expectedSequence: UInt64) throws -> HMuxUsageStreamEvent {
		guard protocolVersion == 1, sequence == expectedSequence else {
			throw HMuxUsageTransportError.invalidFrame
		}
		switch type {
		case "heartbeat":
			guard code == nil, provider == nil, snapshot == nil else {
				throw HMuxUsageTransportError.invalidFrame
			}
			return .heartbeat
		case "status":
			guard code == "home_agent_update_required", provider == nil, snapshot == nil else {
				throw HMuxUsageTransportError.invalidFrame
			}
			return .homeAgentUpdateRequired
		case "snapshot":
			guard code == nil, let provider, let snapshot else {
				throw HMuxUsageTransportError.invalidFrame
			}
			return .snapshot(try snapshot.validated(for: provider))
		default:
			throw HMuxUsageTransportError.invalidFrame
		}
	}
}

enum HMuxUsageStreamEvent: Sendable {
	case heartbeat
	case homeAgentUpdateRequired
	case snapshot(HMuxUsageSnapshot)
}

struct HMuxUsageNDJSONParser {
	static let maximumFrameBytes = 1 << 20
	private var line = Data()

	mutating func consume(_ byte: UInt8) throws -> Data? {
		if byte == 0x0a {
			defer { line.removeAll(keepingCapacity: true) }
			if line.last == 0x0d { line.removeLast() }
			guard !line.isEmpty else { throw HMuxUsageTransportError.invalidFrame }
			return line
		}
		guard line.count < Self.maximumFrameBytes - 1 else {
			throw HMuxUsageTransportError.invalidFrame
		}
		line.append(byte)
		return nil
	}
}

private enum HMuxHomeUsageStream {
	typealias EventHandler = @Sendable (HMuxUsageStreamEvent) async -> Void

	static func run(event: @escaping EventHandler) async throws {
		try Task.checkCancellation()
		let runtime = try HMuxUsageProcessRuntime.start()
		do {
			try await withTaskCancellationHandler {
				var parser = HMuxUsageNDJSONParser()
				var expectedSequence: UInt64 = 1
				let decoder = JSONDecoder()
				for try await byte in runtime.output.bytes {
					try Task.checkCancellation()
					guard let data = try parser.consume(byte) else { continue }
					let frame = try decoder.decode(HMuxUsageStreamFrame.self, from: data)
					await event(try frame.validated(expectedSequence: expectedSequence))
					guard expectedSequence < UInt64.max else {
						throw HMuxUsageTransportError.invalidFrame
					}
					expectedSequence += 1
				}
				throw HMuxUsageTransportError.streamEnded
			} onCancel: {
				runtime.requestStop()
			}
		} catch {
			await runtime.shutdown()
			throw error
		}
	}
}

private final class HMuxUsageProcessRuntime: @unchecked Sendable {
	private let process: Process
	let output: FileHandle
	private let shutdownLock = NSLock()
	private var exitTask: Task<Void, Never>?
	private var shutdownTask: Task<Void, Never>?

	private init(process: Process, output: FileHandle) {
		self.process = process
		self.output = output
	}

	static func start() throws -> HMuxUsageProcessRuntime {
		let executable = try HMuxUsageBackendRuntime.executable()
		let process = Process()
		process.executableURL = executable
		process.arguments = ["--no-update-check", "app", "usage-stream"]
		process.environment = HMuxUsageBackendRuntime.environment()
		let outputPipe = Pipe()
		process.standardInput = FileHandle.nullDevice
		process.standardOutput = outputPipe
		process.standardError = FileHandle.nullDevice
		try process.run()
		return HMuxUsageProcessRuntime(process: process, output: outputPipe.fileHandleForReading)
	}

	func requestStop() {
		shutdownLock.withLock {
			guard shutdownTask == nil else { return }
			try? output.close()
			if process.isRunning { process.terminate() }
			let exitTask = exitTaskLocked()
			let process = process
			shutdownTask = Task.detached(priority: .utility) {
				let deadline = Date().addingTimeInterval(2)
				while process.isRunning, Date() < deadline { usleep(20_000) }
				if process.isRunning { _ = Darwin.kill(process.processIdentifier, SIGKILL) }
				await exitTask.value
			}
		}
	}

	private func exitTaskLocked() -> Task<Void, Never> {
		if let exitTask { return exitTask }
		let process = process
		let task = Task.detached(priority: .utility) { process.waitUntilExit() }
		exitTask = task
		return task
	}

	func shutdown() async {
		requestStop()
		let task = shutdownLock.withLock { shutdownTask }
		await task?.value
	}
}

private enum HMuxUsageBackendRuntime {
	static func executable() throws -> URL {
#if DEBUG
		if let override = ProcessInfo.processInfo.environment["HMUX_EXECUTABLE"],
			override.hasPrefix("/"),
			FileManager.default.isExecutableFile(atPath: override)
		{
			return URL(fileURLWithPath: override)
		}
#endif
		let candidate = Bundle.main.bundleURL
			.appendingPathComponent("Contents/Helpers/hmux", isDirectory: false)
		guard let values = try? candidate.resourceValues(
			forKeys: [.isRegularFileKey, .isSymbolicLinkKey]
		),
		values.isRegularFile == true,
		values.isSymbolicLink != true,
		FileManager.default.isExecutableFile(atPath: candidate.path)
		else { throw HMuxUsageTransportError.helperUnavailable }
		return candidate
	}

	static func environment() -> [String: String] {
		var result = ProcessInfo.processInfo.environment
		for key in ["TMUX", "TMUX_PANE", "HMUX_LAUNCHER", "HMUX_LAUNCHER_ID"] {
			result.removeValue(forKey: key)
		}
		result["HMUX_USAGE_PARENT_PID"] = String(ProcessInfo.processInfo.processIdentifier)
		return result
	}
}

private enum HMuxUsageTransportError: Error {
    case invalidFrame
	case helperUnavailable
	case streamEnded
}

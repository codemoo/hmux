import Foundation
import GhosttyKit
import AppKit
import os

struct HMuxReadyFileTransfer: Equatable {
	let operationID: UUID
	let session: HMuxSessionIdentity
	let surfaceID: UUID
	let result: HMuxFileStageResult
}

enum HMuxFileTransferState: Equatable {
	case idle
	case staging(operationID: UUID, fileCount: Int)
	case ready(HMuxReadyFileTransfer)
	case failed(operationID: UUID?, message: String)

	var operationID: UUID? {
		switch self {
		case .idle: return nil
		case .staging(let operationID, _): return operationID
		case .ready(let transfer): return transfer.operationID
		case .failed(let operationID, _): return operationID
		}
	}
}

private extension Notification.Name {
	static let hmuxFileDrop = Notification.Name("dev.hmux.file-drop")
	static let hmuxFileDropFailed = Notification.Name("dev.hmux.file-drop-failed")
}

@MainActor
final class HMuxTerminalTab: ObservableObject, Identifiable {
    let id: String
    @Published var session: HMuxSession
    @Published var isMissing = false
	@Published var isConnectionClosed = false
	@Published var fileTransferState: HMuxFileTransferState = .idle
    let surfaceView: Ghostty.SurfaceView
	var fileTransferTask: Task<Void, Never>?

    init(session: HMuxSession, ghostty: Ghostty.App) throws {
        self.id = session.identity
        self.session = session

        var configuration = Ghostty.SurfaceConfiguration()
        configuration.command = "hmux app terminal"
        configuration.context = GHOSTTY_SURFACE_CONTEXT_TAB
        let helpers = try HMuxBackend.helperDirectory().path
        let inheritedPath = ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin"
        var environmentVariables = [
            "PATH": helpers + ":" + inheritedPath,
            "HMUX_SESSION_ID": session.id,
            "HMUX_SESSION_CREATED_AT": String(session.createdAt),
        ]
        environmentVariables.merge(HMuxBackend.appEnvironmentMetadata()) { _, appValue in appValue }
        configuration.environmentVariables = environmentVariables
        guard let app = ghostty.app else { throw HMuxBackendError.terminalUnavailable }
        let candidate = Ghostty.SurfaceView(app, baseConfig: configuration)
        guard candidate.surface != nil, candidate.error == nil else {
            throw HMuxBackendError.terminalUnavailable
        }
        surfaceView = candidate
        HMuxSurfaceRegistry.shared.register(surfaceView)
    }

    deinit {
		fileTransferTask?.cancel()
        let surface = surfaceView
        Task { @MainActor in HMuxSurfaceRegistry.shared.unregister(surface) }
    }
}

@MainActor
final class HMuxSessionRowState: ObservableObject, Identifiable {
    let id: String
    @Published var session: HMuxSession

    init(session: HMuxSession) {
        id = session.identity
        self.session = session
    }
}

private enum HMuxInteractionLease: Hashable {
    case liveScroll
    case menuTracking
	case pointerTracking
    case sidebarSearch
    case quickSwitcher
    case management(String)
}

private enum HMuxRestartError: LocalizedError {
	case launcherFailed
	case replacementNotReady

	var errorDescription: String? {
		switch self {
		case .launcherFailed: return "Launch Services could not start the updated HMux app."
		case .replacementNotReady: return "The updated HMux app did not become ready in time."
		}
	}
}

struct HMuxClosedTab: Equatable {
	let identity: String
	let index: Int
}

@MainActor
final class HMuxStore: ObservableObject {
    private static let logger = Logger(subsystem: "dev.hmux.app", category: "store")
	let usageStore = HMuxTokenUsageStore()
	let hostMetricsStore = HMuxHostMetricsStore()

    @Published private(set) var sessionRows: [HMuxSessionRowState] = []
    @Published private(set) var tabs: [HMuxTerminalTab] = []
    @Published var isConversationPresented = false
    @Published var selectedTabID: String?
    @Published var selectedCatalogID: String?
    @Published var searchText = ""
    @Published var sessionFilter: HMuxSessionFilter = .all
    @Published var isInspectorPresented = false
	@Published var isSidebarPresented = true
	@Published private(set) var sidebarSearchFocusRequest: UInt64 = 0
    @Published var isQuickSwitcherPresented = false
    @Published var aliasEditorSession: HMuxSession?
    @Published var terminationSession: HMuxSession?
    @Published var isHiddenManagerPresented = false
	@Published var isCreateSessionPresented = false
	@Published private(set) var availableProfiles: [HMuxProfile] = []
	@Published private(set) var isLoadingProfiles = false
	@Published private(set) var profileLoadErrorMessage: String?
	@Published private(set) var isCreatingSession = false
    @Published private(set) var isRefreshing = false
    @Published private(set) var pendingMutationIDs = Set<String>()
    @Published private(set) var catalogErrorMessage: String?
    @Published private(set) var actionErrorMessage: String?
	private(set) var lastRefreshAt: Date?
    @Published private(set) var appUpdate: HMuxAppUpdate?

    private let ghostty: Ghostty.App
	private var transportTask: Task<Void, Never>?
	private var offlineTransitionTask: Task<Void, Never>?
	private var refreshTask: Task<Void, Never>?
	private var mutationTasks: [String: Task<Void, Never>] = [:]
	private var nativeUpdateTask: Task<Void, Never>?
	private var restartTask: Task<Void, Never>?
	private var profileTask: Task<Void, Never>?
	private var createSessionTask: Task<Void, Never>?
	private var pendingCreatedSession: (profileID: String, name: String, creation: HMuxSessionCreation)?
	@Published private(set) var hasCreatedSessionToOpen = false
	private var terminalFocusTask: Task<Void, Never>?
    private var recoveryRetryTask: Task<Void, Never>?
	private var recentTabIDs: [String] = []
	@Published private(set) var recentlyClosedTabs: [HMuxClosedTab] = []
    private var closeObserver: NSObjectProtocol?
    private var interactionObservers: [NSObjectProtocol] = []
    private var interactionEventMonitor: Any?
	private var latestSessions: [String: HMuxSession] = [:]
	private var catalogSessions: [String: HMuxSession] = [:]
	private var aliasOverlays: [String: HMuxMutationFence<String?>] = [:]
	private var hiddenOverlays: [String: HMuxMutationFence<Bool>] = [:]
	private var aliasConfirmationTasks: [String: Task<Void, Never>] = [:]
	private var hiddenConfirmationTasks: [String: Task<Void, Never>] = [:]
	private var mutationToken: UInt64 = 0
	private var deferredSessions: [HMuxSession]?
	private var deferredApplyScheduled = false
	private var interactionLeaseCounts: [HMuxInteractionLease: Int] = [:]
	private var refreshInFlightID: UInt64?
	private var refreshOperationID: UInt64 = 0
    private var refreshQueued = false
    private var refreshQueuedShowsProgress = false
	private var refreshQueuedGeneration: UInt64?
	private var disconnectedAt: Date?
	private var transportGeneration: UInt64 = 0
	private var activeWebSocketGeneration: UInt64?
	private var acceptedCatalogRevision: UInt64 = 0
	private var acceptedCatalogStamp: HMuxCatalogStamp?
	private var selectionEpoch: UInt64 = 0
	private var lifecycleGeneration: UInt64 = 0
	private var workspaceSourceKey: String?
	private var isWindowVisible = true
	private var workspaceRestorationComplete = false
	private var isRestoringWorkspace = false

    @Published private(set) var sharedWorkspaceStatus = "Shared tabs: connecting"
    private var sharedWorkspaceTask: Task<Void, Never>?
    private var sharedWorkspaceBusy = false
    private var sharedWorkspaceLoaded = false
    private var sharedWorkspaceDirty = false
    private var sharedWorkspaceRevision: UInt64 = 0
    private var sharedWorkspaceEdit: UInt64 = 0
    private var sharedWorkspaceBase: [HMuxSessionIdentity] = []
    private var sharedWorkspacePending: HMuxSharedWorkspaceChange?
    private var sharedWorkspacePendingEdit: UInt64 = 0
    private var sharedWorkspaceLastTabs: [HMuxSessionIdentity] = []
    private var sharedWorkspaceLastSelected: HMuxSessionIdentity?
	private let workspacePersistence = HMuxWorkspacePersistence(defaults: .standard)
	@Published private(set) var catalogConnectionState: HMuxCatalogConnectionState = .starting
#if DEBUG
    private var uiTestIdentity: String?
    private var uiTestCloseDelayMilliseconds: UInt64?
#endif

    init(ghostty: Ghostty.App) {
        self.ghostty = ghostty
		HMuxBackend.bindWorkspaceSource(nil)
#if DEBUG
        let environment = ProcessInfo.processInfo.environment
        if let id = environment["HMUX_UI_TEST_SESSION_ID"], id.hasPrefix("$"),
           let createdAtText = environment["HMUX_UI_TEST_SESSION_CREATED_AT"],
           let createdAt = Int64(createdAtText), createdAt > 0 {
            uiTestIdentity = "\(id):\(createdAt)"
            Self.logger.info("UI test automation configured")
        }
        if let delayText = environment["HMUX_UI_TEST_CLOSE_AFTER_MS"],
           let delay = UInt64(delayText), delay > 0, delay <= 30_000 {
            uiTestCloseDelayMilliseconds = delay
        }
#endif
        closeObserver = NotificationCenter.default.addObserver(
            forName: Ghostty.Notification.ghosttyCloseSurface,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let surface = notification.object as? Ghostty.SurfaceView else { return }
            Task { @MainActor in self?.connectionDidClose(surface: surface) }
        }
		observeInteraction(
			start: NSScrollView.willStartLiveScrollNotification,
			end: NSScrollView.didEndLiveScrollNotification,
			lease: .liveScroll
		)
		observeInteraction(
			start: NSMenu.didBeginTrackingNotification,
			end: NSMenu.didEndTrackingNotification,
			lease: .menuTracking
		)
		interactionEventMonitor = NSEvent.addLocalMonitorForEvents(
			matching: [.leftMouseDown, .leftMouseUp]
		) { [weak self] event in
			let active: Bool?
			switch event.type {
			case .leftMouseDown:
				active = true
			case .leftMouseUp:
				active = false
			default:
				active = nil
			}
			if let active {
				MainActor.assumeIsolated {
					if active { self?.terminalFocusTask?.cancel() }
					self?.setInteraction(.pointerTracking, active: active)
				}
			}
			return event
		}
		interactionObservers.append(NotificationCenter.default.addObserver(
			forName: NSApplication.didResignActiveNotification,
			object: nil,
			queue: .main
		) { [weak self] _ in
			MainActor.assumeIsolated {
				self?.selectionEpoch &+= 1
				self?.clearTransientInteractions()
			}
		})
		interactionObservers.append(NotificationCenter.default.addObserver(
			forName: .hmuxFileDropFailed,
			object: nil,
			queue: .main
		) { [weak self] notification in
			guard let surface = notification.object as? Ghostty.SurfaceView,
			      let operationID = notification.userInfo?["operation_id"] as? UUID else { return }
			MainActor.assumeIsolated {
				self?.failFileDrop(operationID: operationID, surface: surface)
			}
		})
		interactionObservers.append(NotificationCenter.default.addObserver(
			forName: .hmuxFileDrop,
			object: nil,
			queue: .main
		) { [weak self] notification in
			guard let surface = notification.object as? Ghostty.SurfaceView,
			      let operationID = notification.userInfo?["operation_id"] as? UUID,
			      let urls = notification.userInfo?["urls"] as? [URL] else { return }
			MainActor.assumeIsolated {
				self?.beginFileDrop(operationID: operationID, urls: urls, surface: surface)
			}
		})
    }

    deinit {
		Task { @MainActor [usageStore] in usageStore.stop() }
        sharedWorkspaceTask?.cancel()
		transportTask?.cancel()
		offlineTransitionTask?.cancel()
		refreshTask?.cancel()
		mutationTasks.values.forEach { $0.cancel() }
		nativeUpdateTask?.cancel()
		restartTask?.cancel()
		profileTask?.cancel()
		createSessionTask?.cancel()
		terminalFocusTask?.cancel()
        recoveryRetryTask?.cancel()
		aliasConfirmationTasks.values.forEach { $0.cancel() }
		hiddenConfirmationTasks.values.forEach { $0.cancel() }
        if let closeObserver { NotificationCenter.default.removeObserver(closeObserver) }
        for observer in interactionObservers { NotificationCenter.default.removeObserver(observer) }
		if let interactionEventMonitor { NSEvent.removeMonitor(interactionEventMonitor) }
    }

    var sessions: [HMuxSession] { sessionRows.map(\.session) }
    var visibleSessions: [HMuxSession] { sessions.filter { !$0.isHidden } }
    var hiddenRows: [HMuxSessionRowState] { sessionRows.filter { $0.session.isHidden } }
    var attentionSummary: HMuxAttentionSummary { HMuxAttentionSummary(sessions: visibleSessions) }
    var isInitialLoading: Bool { lastRefreshAt == nil && catalogErrorMessage == nil }
    var isInitialFailure: Bool { lastRefreshAt == nil && catalogErrorMessage != nil }
	var isCatalogOffline: Bool { catalogConnectionState == .offline }
	var isCatalogReconnecting: Bool { catalogConnectionState == .reconnecting }
	var canCreateSession: Bool {
		workspaceSourceKey != nil && isCatalogConnected && !isCatalogReconnecting &&
			tabs.count < HMuxWorkspaceSnapshot.maximumOpenSessions && !isCreatingSession
	}
	var createSessionUnavailableReason: String? {
		if workspaceSourceKey == nil || !isCatalogConnected || isCatalogReconnecting {
			return "Connect to Home before starting a session."
		}
		if tabs.count >= HMuxWorkspaceSnapshot.maximumOpenSessions {
			return "Close a tab before opening another. Your Home session will keep running."
		}
		return isCreatingSession ? "A session is already opening." : nil
	}
	var isCatalogConnected: Bool {
		switch catalogConnectionState {
		case .connected, .reconnecting: return lastRefreshAt != nil
		default: return false
		}
	}
	var connectionLabel: String {
		if isInitialFailure || isCatalogOffline { return "Offline" }
		if isCatalogReconnecting { return "Reconnecting…" }
		return isCatalogConnected ? "Connected" : "Connecting…"
    }

    var filteredSessionRows: [HMuxSessionRowState] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return sessionRows.filter { row in
            let session = row.session
            if session.isHidden { return false }
            switch sessionFilter {
            case .all: break
            case .active where !session.hmuxIsActive: return false
            case .attention where !session.hmuxNeedsAttention: return false
            default: break
            }
            return query.isEmpty || matches(session, query: query)
        }
    }

    func sessionRows(matching rawQuery: String) -> [HMuxSessionRowState] {
        let query = rawQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
		let visibleRows = sessionRows.filter { !$0.session.isHidden }
        guard !query.isEmpty else { return visibleRows }
        return visibleRows.filter { matches($0.session, query: query) }
    }

    func hiddenRows(matching rawQuery: String) -> [HMuxSessionRowState] {
        let query = rawQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !query.isEmpty else { return hiddenRows }
        return hiddenRows.filter { matches($0.session, query: query) }
    }


    var selectedTab: HMuxTerminalTab? {
        tabs.first { $0.id == selectedTabID }
    }
	var openTabIDs: Set<String> { Set(tabs.map(\.id)) }
	var hasPresentedModal: Bool {
		isQuickSwitcherPresented || isCreateSessionPresented || isHiddenManagerPresented ||
			aliasEditorSession != nil || terminationSession != nil
	}
	var canReopenClosedTab: Bool {
		isCatalogConnected && recentlyClosedTabs.contains { latestSessions[$0.identity] != nil }
	}

	func start() {
		lifecycleGeneration &+= 1
		usageStore.start()
		hostMetricsStore.start()
		checkForNativeUpdate()
		transportGeneration &+= 1
		let generation = transportGeneration
		transportTask?.cancel()
		offlineTransitionTask?.cancel()
		offlineTransitionTask = nil
		disconnectedAt = nil
		activeWebSocketGeneration = nil
		setCatalogConnectionState(.starting)
		transportTask = Task { [weak self] in
			await self?.runCatalogTransport(generation: generation)
		}
    }

	func stop() {
		persistWorkspace()
        sharedWorkspaceTask?.cancel()
        sharedWorkspaceTask = nil
		terminalFocusTask?.cancel()
        recoveryRetryTask?.cancel()
        recoveryRetryTask = nil
		lifecycleGeneration &+= 1
		usageStore.stop()
		hostMetricsStore.stop()
		transportGeneration &+= 1
		transportTask?.cancel()
		transportTask = nil
		offlineTransitionTask?.cancel()
		offlineTransitionTask = nil
		refreshTask?.cancel()
		refreshTask = nil
		refreshOperationID &+= 1
		refreshInFlightID = nil
		refreshQueued = false
		refreshQueuedShowsProgress = false
		refreshQueuedGeneration = nil
		isRefreshing = false
		mutationTasks.values.forEach { $0.cancel() }
		mutationTasks.removeAll()
		pendingMutationIDs.removeAll()
		aliasConfirmationTasks.values.forEach { $0.cancel() }
		aliasConfirmationTasks.removeAll()
		hiddenConfirmationTasks.values.forEach { $0.cancel() }
		hiddenConfirmationTasks.removeAll()
		aliasOverlays.removeAll()
		hiddenOverlays.removeAll()
		deferredSessions = nil
		deferredApplyScheduled = false
		nativeUpdateTask?.cancel()
		nativeUpdateTask = nil
		restartTask?.cancel()
		restartTask = nil
		profileTask?.cancel()
		profileTask = nil
		createSessionTask?.cancel()
		createSessionTask = nil
		isLoadingProfiles = false
		isCreatingSession = false
		for tab in tabs {
			tab.fileTransferTask?.cancel()
			tab.fileTransferTask = nil
			if case .staging = tab.fileTransferState { tab.fileTransferState = .idle }
		}
	}

    func refresh() {
		// A visible Retry must also recover a stalled stream or rejected bootstrap.
		// A poll alone is suppressed while the WebSocket generation owns the catalog.
		transportGeneration &+= 1
		let generation = transportGeneration
		transportTask?.cancel()
		refreshTask?.cancel()
		refreshTask = nil
		refreshInFlightID = nil
		refreshQueued = false
		refreshQueuedShowsProgress = false
		refreshQueuedGeneration = nil
		isRefreshing = false
		offlineTransitionTask?.cancel()
		offlineTransitionTask = nil
		disconnectedAt = nil
		activeWebSocketGeneration = nil
		catalogErrorMessage = nil
		setCatalogConnectionState(lastRefreshAt == nil ? .awaitingInitialSnapshot : .reconnecting)
		transportTask = Task { [weak self] in
			await self?.runCatalogTransport(generation: generation)
		}
	}

	func focusSidebarSearch() {
		guard !hasPresentedModal else { return }
		terminalFocusTask?.cancel()
		isSidebarPresented = true
		sidebarSearchFocusRequest &+= 1
	}

	func consumeSidebarSearchFocusRequest() { sidebarSearchFocusRequest = 0 }

	private func runCatalogTransport(generation: UInt64) async {
		var consecutiveFailures = 0
		while isCurrentTransport(generation) {
			var connection: HMuxCatalogStreamConnection?
			var connectedAt: Date?
			do {
				if lastRefreshAt == nil, catalogConnectionState != .offline {
					setCatalogConnectionState(.awaitingInitialSnapshot)
				}
				connection = try await HMuxCatalogStreamConnection.open()
				connectedAt = Date()
				guard isCurrentTransport(generation) else {
					await connection?.close()
					return
				}
				while isCurrentTransport(generation), let connection {
					let catalog = try await connection.receiveCatalog()
					guard isCurrentTransport(generation) else {
						await connection.close()
						return
					}
					acceptCatalog(catalog, mode: .webSocket, generation: generation)
					if activeWebSocketGeneration == nil { activeWebSocketGeneration = generation }
				}
				if !isCurrentTransport(generation) {
					await connection?.close()
					return
				}
			} catch HMuxBackendError.streamUnsupported {
				await connection?.close()
				if activeWebSocketGeneration == generation { activeWebSocketGeneration = nil }
				guard isCurrentTransport(generation) else { return }
				await runPollingFallback(generation: generation)
				return
			} catch {
				await connection?.close()
				if activeWebSocketGeneration == generation { activeWebSocketGeneration = nil }
				guard isCurrentTransport(generation) else { return }
				let connectionLifetime = connectedAt.map { max(0, Date().timeIntervalSince($0)) } ?? 0
				consecutiveFailures = hmuxNextCatalogStreamFailureCount(
					previous: consecutiveFailures,
					connectionLifetime: connectionLifetime
				)
				noteTransportFailure(error, mode: .webSocket, generation: generation)
				if consecutiveFailures >= 3 {
					// Repeated stream/bootstrap failures usually indicate an older
					// agent command surface. Enter the same quiet, bounded fallback
					// used by an explicit unsupported bootstrap until app restart.
					await runPollingFallback(generation: generation)
					return
				}
				let baseDelay = consecutiveFailures >= 3
					? 15.0
					: min(8.0, 0.5 * pow(2.0, Double(consecutiveFailures - 1)))
				let jitter = Double.random(in: 0...min(0.75, baseDelay * 0.15))
				do {
					try await Task.sleep(nanoseconds: UInt64((baseDelay + jitter) * 1_000_000_000))
				} catch {
					return
				}
			}
		}
	}

	private func runPollingFallback(generation: UInt64) async {
		while isCurrentTransport(generation) {
			requestRefresh(showProgress: false, mode: .pollingFallback, generation: generation)
			do {
				try await Task.sleep(nanoseconds: 15_000_000_000)
			} catch {
				return
			}
		}
	}

	private func acceptCatalog(
		_ catalog: HMuxCatalog,
		mode: HMuxCatalogTransportMode,
		generation: UInt64
	) {
		guard isCurrentTransport(generation) else { return }
		guard hmuxShouldAcceptWorkspaceSource(current: workspaceSourceKey, incoming: catalog.workspaceSourceKey) else {
			let message = catalog.workspaceSourceKey == nil
				? "The Home connection could not be verified. Check the connection settings and reopen HMux."
				: "The Home connection settings changed. Reopen HMux to use the new connection."
			noteTransportFailure(HMuxBackendError.backend(message), mode: mode, generation: generation)
			return
		}
		guard hmuxShouldAcceptCatalogResult(
			generation: generation,
			currentGeneration: transportGeneration,
			activeWebSocketGeneration: activeWebSocketGeneration,
			mode: mode,
			startingRevision: acceptedCatalogRevision,
			currentRevision: acceptedCatalogRevision
		) else { return }
		if workspaceSourceKey == nil, let key = catalog.workspaceSourceKey {
			workspaceSourceKey = key
			HMuxBackend.bindWorkspaceSource(key)
		}
		guard acceptCatalogProjection(catalog) else { return }
		let mergedUpdate = hmuxMergedAppUpdate(current: appUpdate, incoming: catalog.appUpdate, mode: mode)
		if appUpdate != mergedUpdate { appUpdate = mergedUpdate }
		if catalogErrorMessage != nil { catalogErrorMessage = nil }
		let now = Date()
		hostMetricsStore.receive(catalog.hostMetrics, receivedAt: now)
		offlineTransitionTask?.cancel()
		offlineTransitionTask = nil
		disconnectedAt = nil
		lastRefreshAt = now
		setCatalogConnectionState(.connected(mode))
		startSharedWorkspaceSync()
		Self.logger.debug("Catalog transport received \(catalog.sessions.count, privacy: .public) sessions")
	}

	@discardableResult
	private func acceptCatalogProjection(_ catalog: HMuxCatalog) -> Bool {
		guard hmuxShouldAcceptWorkspaceSource(current: workspaceSourceKey, incoming: catalog.workspaceSourceKey),
		      let stamp = HMuxCatalogStamp(catalog.generatedAt),
		      hmuxShouldAcceptCatalogStamp(stamp, after: acceptedCatalogStamp) else { return false }
		if stamp == acceptedCatalogStamp { return true }
		acceptedCatalogStamp = stamp
		acceptedCatalogRevision &+= 1
		catalogSessions = catalog.sessions.reduce(into: [:]) { result, session in
			if result[session.identity] == nil { result[session.identity] = session }
		}
		_ = applyOrDefer(catalog.sessions)
		return true
	}

	private func noteTransportFailure(
		_ error: Error,
		mode: HMuxCatalogTransportMode,
		generation: UInt64
	) {
		guard isCurrentTransport(generation) else { return }
		guard hmuxShouldAcceptCatalogResult(
			generation: generation,
			currentGeneration: transportGeneration,
			activeWebSocketGeneration: activeWebSocketGeneration,
			mode: mode,
			startingRevision: acceptedCatalogRevision,
			currentRevision: acceptedCatalogRevision
		) else { return }
		Self.logger.error("Catalog transport failed")
		let now = Date()
		if disconnectedAt == nil { disconnectedAt = now }
		let outageStartedAt = disconnectedAt ?? now
		let message = error.localizedDescription
		if hmuxCatalogOutageShouldBeOffline(
			hasReceivedCatalog: lastRefreshAt != nil,
			disconnectedAt: outageStartedAt,
			now: now
		) {
			markCatalogOffline(message: message)
			return
		}
		setCatalogConnectionState(.reconnecting)
		scheduleOfflineTransition(
			generation: generation,
			outageStartedAt: outageStartedAt,
			message: message
		)
	}

	private func scheduleOfflineTransition(
		generation: UInt64,
		outageStartedAt: Date,
		message: String
	) {
		guard offlineTransitionTask == nil else { return }
		offlineTransitionTask = Task { [weak self] in
			do {
				try await Task.sleep(nanoseconds: 20_000_000_000)
			} catch {
				return
			}
			guard let self,
			      self.transportGeneration == generation,
			      self.disconnectedAt == outageStartedAt else { return }
			self.offlineTransitionTask = nil
			self.markCatalogOffline(message: message)
		}
	}

	private func markCatalogOffline(message: String) {
		offlineTransitionTask?.cancel()
		offlineTransitionTask = nil
		setCatalogConnectionState(.offline)
		hostMetricsStore.markOffline()
		if catalogErrorMessage != message { catalogErrorMessage = message }
	}

	private func requestCatalogRefreshIfNeeded() {
		if activeWebSocketGeneration == transportGeneration { return }
		if case .connected(.webSocket) = catalogConnectionState { return }
		requestRefresh(showProgress: false, mode: .pollingFallback, generation: transportGeneration)
	}

	private func requestRefresh(
		showProgress: Bool,
		mode: HMuxCatalogTransportMode,
		generation: UInt64
	) {
		guard hmuxShouldAcceptCatalogResult(
			generation: generation,
			currentGeneration: transportGeneration,
			activeWebSocketGeneration: activeWebSocketGeneration,
			mode: mode,
			startingRevision: acceptedCatalogRevision,
			currentRevision: acceptedCatalogRevision
		) else { return }
		let startingRevision = acceptedCatalogRevision
		guard refreshInFlightID == nil else {
            refreshQueued = true
            refreshQueuedShowsProgress = refreshQueuedShowsProgress || showProgress
			refreshQueuedGeneration = generation
            return
        }
		refreshOperationID &+= 1
		let operationID = refreshOperationID
		refreshInFlightID = operationID
        if showProgress { isRefreshing = true }
		refreshTask = Task { [weak self] in
			guard let self else { return }
			let result: Result<HMuxCatalog, Error>
			do {
				let catalog = try await HMuxBackend.loadCatalog()
				result = .success(catalog)
			} catch {
				result = .failure(error)
            }
			guard self.refreshInFlightID == operationID else { return }
			if hmuxShouldAcceptCatalogResult(
				generation: generation,
				currentGeneration: self.transportGeneration,
				activeWebSocketGeneration: self.activeWebSocketGeneration,
				mode: mode,
				startingRevision: startingRevision,
				currentRevision: self.acceptedCatalogRevision
			), !Task.isCancelled {
				switch result {
				case .success(let catalog):
					self.acceptCatalog(catalog, mode: mode, generation: generation)
				case .failure(let error):
					self.noteTransportFailure(error, mode: mode, generation: generation)
				}
			}
			self.refreshInFlightID = nil
			self.refreshTask = nil
			if self.isRefreshing { self.isRefreshing = false }
			if self.refreshQueued {
				let queuedShowsProgress = self.refreshQueuedShowsProgress
				let queuedGeneration = self.refreshQueuedGeneration
				self.refreshQueued = false
				self.refreshQueuedShowsProgress = false
				self.refreshQueuedGeneration = nil
				if let queuedGeneration {
					self.requestRefresh(
						showProgress: queuedShowsProgress,
						mode: .pollingFallback,
						generation: queuedGeneration
					)
				}
			}
        }
    }

	private func isCurrentTransport(_ generation: UInt64) -> Bool {
		generation == transportGeneration && !Task.isCancelled
	}

	private func setCatalogConnectionState(_ state: HMuxCatalogConnectionState) {
		if catalogConnectionState != state { catalogConnectionState = state }
	}

	@discardableResult
	func open(_ session: HMuxSession) -> Bool {
        guard sharedWorkspaceLoaded || isRestoringWorkspace else {
            actionErrorMessage = "Wait for shared tabs to load from Home before opening a session."
            return false
        }
		guard workspaceSourceKey != nil else {
			actionErrorMessage = "The Home connection could not be verified yet. Wait for it to reconnect before opening a session."
			return false
		}
        let currentSession = latestSessions[session.identity] ?? session
        if let existing = tabs.first(where: { $0.id == currentSession.identity }) {
            select(existing)
			return true
        }
		guard tabs.count < HMuxWorkspaceSnapshot.maximumOpenSessions else {
			actionErrorMessage = "You have 32 tabs open. Close a tab before opening another; its Home session will keep running."
			return false
		}
        do {
            let tab = try HMuxTerminalTab(session: currentSession, ghostty: ghostty)
            tabs.append(tab)
            select(tab)
			return true
        } catch {
            Self.logger.error("Terminal surface creation failed")
            actionErrorMessage = error.localizedDescription
			return false
        }
    }

    func openSelectedCatalogSession() {
        guard let selectedCatalogID,
              let session = latestSessions[selectedCatalogID] ?? sessionRows.first(where: { $0.id == selectedCatalogID })?.session else { return }
        open(session)
    }

    func selectTab(at index: Int) {
        guard tabs.indices.contains(index) else { return }
        select(tabs[index])
    }

    func select(_ tab: HMuxTerminalTab) {
		guard tabs.contains(where: { $0 === tab }) else { return }
		selectionEpoch &+= 1
        selectedTabID = tab.id
        selectedCatalogID = tab.id
		recentTabIDs.removeAll { $0 == tab.id }
		recentTabIDs.insert(tab.id, at: 0)
		recentTabIDs = Array(recentTabIDs.prefix(64))
		updateSurfaceVisibility()
		requestTerminalFocus()
		persistWorkspace()
    }

	func focusSelectedTerminal() { requestTerminalFocus() }

    func setConversationPresented(_ presented: Bool) {
        isConversationPresented = presented
        terminalFocusTask?.cancel()
        if presented {
            selectedTab?.surfaceView.focusDidChange(false)
            if let window = selectedTab?.surfaceView.window { window.makeFirstResponder(nil) }
            isInspectorPresented = false
        }
        updateSurfaceVisibility()
        if !presented { requestTerminalFocus() }
    }

	func setWindowVisible(_ visible: Bool) {
		isWindowVisible = visible
		updateSurfaceVisibility()
	}

	private func updateSurfaceVisibility() {
		for tab in tabs {
			if tab.id != selectedTabID { tab.surfaceView.focusDidChange(false) }
			guard let surface = tab.surfaceView.surface else { continue }
			let visible = isWindowVisible && tab.id == selectedTabID && !isConversationPresented
			ghostty_surface_set_occlusion(surface, visible)
			if visible { ghostty_surface_refresh(surface) }
		}
	}

	private func requestTerminalFocus() {
		terminalFocusTask?.cancel()
		guard !isConversationPresented, let tab = selectedTab else { return }
		let epoch = selectionEpoch
		terminalFocusTask = Task { [weak self, weak tab] in
			// SwiftUI first needs to mount the selected NSView. Every retry is
			// bound to the current selection and cannot steal a later interaction.
			for delay in [UInt64(0), 30_000_000, 60_000_000, 120_000_000, 240_000_000] {
				if delay == 0 { await Task.yield() }
				else { do { try await Task.sleep(nanoseconds: delay) } catch { return } }
				guard let self, let tab, !Task.isCancelled, self.selectionEpoch == epoch,
				      self.selectedTab === tab, !self.hasPresentedModal, !self.isConversationPresented,
                      self.interactionLeaseCounts[.sidebarSearch] == nil, NSApp.isActive else { return }
				guard let window = tab.surfaceView.window else { continue }
				guard window.isKeyWindow else { return }
				if window.attachedSheet != nil || tab.surfaceView.isHiddenOrHasHiddenAncestor { continue }
				guard window.makeFirstResponder(tab.surfaceView), window.firstResponder === tab.surfaceView else { continue }
				// AppKit can keep the same firstResponder while SwiftUI updates its
				// hosting hierarchy. Reconcile Ghostty's focus flag explicitly.
				tab.surfaceView.focusDidChange(true)
				return
			}
		}
	}

	func reopenClosedTab() {
		guard isCatalogConnected else { return }
		while let closed = recentlyClosedTabs.popLast() {
			guard let session = latestSessions[closed.identity] else { continue }
			guard open(session) else {
				recentlyClosedTabs.append(closed)
				return
			}
			if let index = tabs.firstIndex(where: { $0.id == closed.identity }) {
				let tab = tabs.remove(at: index)
				tabs.insert(tab, at: min(closed.index, tabs.count))
			}
			persistWorkspace()
			return
		}
	}

	func moveTab(_ tab: HMuxTerminalTab, by offset: Int) {
		guard let index = tabs.firstIndex(where: { $0 === tab }), tabs.indices.contains(index + offset) else { return }
		tabs.swapAt(index, index + offset)
		persistWorkspace()
	}

	func persistWorkspace() {
		guard workspaceRestorationComplete, !isRestoringWorkspace, let workspaceSourceKey else { return }
        if sharedWorkspaceLoaded {
            let refs = tabs.map { HMuxSessionIdentity(session: $0.session) }
            let selected = selectedTab.map { HMuxSessionIdentity(session: $0.session) }
            if refs != sharedWorkspaceLastTabs {
                sharedWorkspaceLastTabs = refs
                sharedWorkspaceLastSelected = selected
                sharedWorkspaceEdit &+= 1
                sharedWorkspaceDirty = true
                Task { [weak self] in await self?.syncSharedWorkspace() }
            }
        }
		do {
			let available = tabs.filter { !$0.isMissing }
			let references = available.map { HMuxSessionIdentity(session: $0.session) }
			let selected = selectedTab.flatMap { $0.isMissing ? nil : HMuxSessionIdentity(session: $0.session) }
			let snapshot = try HMuxWorkspaceSnapshot.capture(
				sourceKey: workspaceSourceKey, openSessions: references,
				selectedSession: selected,
				sidebarVisible: isSidebarPresented, inspectorVisible: isInspectorPresented
			)
			try workspacePersistence.save(snapshot)
		} catch {
			Self.logger.error("Workspace layout could not be saved")
		}
	}

	private func restoreWorkspaceIfNeeded(_ sessions: [HMuxSession]) {
		guard !workspaceRestorationComplete, let workspaceSourceKey else { return }
		workspaceRestorationComplete = true
		// A user interaction completed before the first snapshot must take
		// priority over the old layout. Restoration never changes Home work.
		guard tabs.isEmpty else { persistWorkspace(); return }
		isRestoringWorkspace = true
		var restoredSnapshot = false
		defer { isRestoringWorkspace = false; if restoredSnapshot { persistWorkspace() } }
		do {
			guard let snapshot = try workspacePersistence.load(sourceKey: workspaceSourceKey),
			      let plan = snapshot.restorePlan(sourceKey: workspaceSourceKey, currentSessions: sessions) else { return }
			restoredSnapshot = true
			if let visible = plan.sidebarVisible { isSidebarPresented = visible }
			if let visible = plan.inspectorVisible { isInspectorPresented = visible }
			var restored: [HMuxTerminalTab] = []
			for session in plan.sessions {
				do { restored.append(try HMuxTerminalTab(session: session, ghostty: ghostty)) }
				catch { Self.logger.error("A previous workspace tab could not be restored") }
			}
			tabs = restored
			let selected = plan.selectedSession.map { "\($0.id):\($0.createdAt)" }
			if let tab = tabs.first(where: { $0.id == selected }) ?? tabs.first { select(tab) }
			if restored.count < plan.sessions.count {
				actionErrorMessage = "Some previous tabs could not be opened. Their Home sessions remain available in the list."
			}
		} catch {
			Self.logger.error("Previous workspace layout could not be read")
		}
	}

    private func startSharedWorkspaceSync() {
        guard sharedWorkspaceTask == nil else { return }
        sharedWorkspaceTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.syncSharedWorkspace()
                do { try await Task.sleep(nanoseconds: 5_000_000_000) }
                catch { return }
                if self == nil { return }
            }
        }
    }

    private func syncSharedWorkspace() async {
        guard !sharedWorkspaceBusy, let source = workspaceSourceKey, isCatalogConnected else { return }
        sharedWorkspaceBusy = true
        defer { sharedWorkspaceBusy = false }
        let generation = lifecycleGeneration
        do {
            if sharedWorkspaceLoaded && sharedWorkspaceDirty && sharedWorkspacePending == nil {
                sharedWorkspacePending = HMuxSharedWorkspaceChange(
                    operationId: UUID().uuidString, revision: sharedWorkspaceRevision,
                    base: sharedWorkspaceBase,
                    tabs: tabs.map { HMuxSessionIdentity(session: $0.session) },
                    selected: nil
                )
                sharedWorkspacePendingEdit = sharedWorkspaceEdit
            }
            let sent = sharedWorkspacePending
            let value = try await HMuxBackend.sharedWorkspace(change: sent)
            guard !Task.isCancelled, generation == lifecycleGeneration, source == workspaceSourceKey else { return }
            sharedWorkspaceRevision = value.revision
            if value.conflict != nil {
                sharedWorkspacePending = nil
                sharedWorkspaceDirty = false
                applySharedWorkspace(value)
                sharedWorkspaceStatus = "Shared tabs: Home layout restored"
                actionErrorMessage = "Shared tabs changed or reached their limit. The current Home layout was restored; recent local tab changes could not be saved."
                return
            }
            if let sent {
                sharedWorkspaceBase = sent.tabs
                sharedWorkspacePending = nil
                sharedWorkspaceDirty = sharedWorkspaceEdit != sharedWorkspacePendingEdit
            }
            if !sharedWorkspaceLoaded && !value.initialized {
                restoreWorkspaceIfNeeded(Array(latestSessions.values))
                sharedWorkspaceLoaded = true
                sharedWorkspaceBase = []
                sharedWorkspaceDirty = !tabs.isEmpty
                sharedWorkspaceLastTabs = tabs.map { HMuxSessionIdentity(session: $0.session) }
                sharedWorkspaceLastSelected = selectedTab.map { HMuxSessionIdentity(session: $0.session) }
            } else if !sharedWorkspaceDirty {
                applySharedWorkspace(value)
            }
            sharedWorkspaceStatus = sharedWorkspaceDirty ? "Shared tabs: syncing" : "Shared tabs: synced"
            if sharedWorkspaceDirty {
                Task { [weak self] in await self?.syncSharedWorkspace() }
            }
        } catch {
            guard !Task.isCancelled, generation == lifecycleGeneration else { return }
            sharedWorkspaceStatus = "Shared tabs: waiting to sync"

        }
    }

    private func applySharedWorkspace(_ value: HMuxSharedWorkspace) {
        let first = !sharedWorkspaceLoaded
        let previous = selectedTabID
        isRestoringWorkspace = true
        defer { isRestoringWorkspace = false }
        let identities = value.tabs.map { "\($0.id):\($0.createdAt)" }
        for tab in tabs where !identities.contains(tab.id) { close(id: tab.id, remember: false) }
        for reference in value.tabs {
            let id = "\(reference.id):\(reference.createdAt)"
            guard !tabs.contains(where: { $0.id == id }) else { continue }
            let live = latestSessions[id]
            let session = live ?? HMuxSession(
                id: reference.id, name: "Session unavailable", alias: nil, hidden: nil,
                createdAt: reference.createdAt, activityAt: 0, attachedClients: 0, windowCount: 0,
                windowNames: [], activeWindow: "", currentPath: "", currentCommand: "",
                profile: nil, label: nil, tags: nil, kind: nil, runtime: nil, model: nil,
                state: nil, process: nil, workingSince: nil, workflow: nil, workflows: nil,
                hostAlias: nil, width: nil, height: nil
            )
            do {
                let tab = try HMuxTerminalTab(session: session, ghostty: ghostty)
                tab.isMissing = live == nil
                tabs.append(tab)
            } catch { sharedWorkspaceStatus = "Shared tabs: a terminal could not open" }
        }
        let current = Dictionary(uniqueKeysWithValues: tabs.map { ($0.id, $0) })
        tabs = identities.compactMap { current[$0] }
        let localSnapshot = first ? workspaceSourceKey.flatMap { try? workspacePersistence.load(sourceKey: $0) } : nil
        let remoteSelected = localSnapshot?.selectedSession.map { "\($0.id):\($0.createdAt)" }
        if let visible = localSnapshot?.sidebarVisible { isSidebarPresented = visible }
        if let visible = localSnapshot?.inspectorVisible { isInspectorPresented = visible }
        let wanted = !first && previous.map({ current[$0] != nil }) == true ? previous : remoteSelected
        if let chosen = tabs.first(where: { $0.id == wanted }) ?? tabs.first, chosen.id != selectedTabID {
            select(chosen)
        }
        // Base includes only rendered tabs. Failed surface creation is not a
        // local close and must never remove the corresponding Home reference.
        sharedWorkspaceBase = tabs.map { HMuxSessionIdentity(session: $0.session) }
        sharedWorkspaceLastTabs = sharedWorkspaceBase
        sharedWorkspaceLastSelected = selectedTab.map { HMuxSessionIdentity(session: $0.session) }
        sharedWorkspaceLoaded = true
        workspaceRestorationComplete = true
    }

    func closeSelectedTab() {
        guard let selectedTab else { return }
        close(selectedTab)
    }

    func selectNextTab() { selectTab(offset: 1) }
    func selectPreviousTab() { selectTab(offset: -1) }

    func focusNextAttentionSession() {
        let candidates = sessionRows.filter { !$0.session.isHidden && $0.session.hmuxNeedsAttention }
        guard !candidates.isEmpty else { return }
        let currentIndex = candidates.firstIndex { $0.id == selectedTabID }
        let nextIndex = currentIndex.map { ($0 + 1) % candidates.count } ?? 0
        open(candidates[nextIndex].session)
    }

    func dismissActionError() { actionErrorMessage = nil }

	func presentQuickSwitcher() {
		guard !hasPresentedModal else { return }
		terminalFocusTask?.cancel()
		isQuickSwitcherPresented = true
	}

	func presentHiddenSessions() {
		guard !hasPresentedModal else { return }
		terminalFocusTask?.cancel()
		isHiddenManagerPresented = true
	}

	func beginSessionCreation() {
		guard !hasPresentedModal else { return }
		guard canCreateSession else {
			actionErrorMessage = createSessionUnavailableReason
			return
		}
		terminalFocusTask?.cancel()
		isCreateSessionPresented = true
		loadProfiles()
	}

	func endSessionCreation() {
		pendingCreatedSession = nil
		hasCreatedSessionToOpen = false
		focusSelectedTerminal()
	}

	func loadProfiles(force: Bool = false) {
		guard !isLoadingProfiles else { return }
		if !force, !availableProfiles.isEmpty { return }
		profileTask?.cancel()
		isLoadingProfiles = true
		profileLoadErrorMessage = nil
		let generation = lifecycleGeneration
		profileTask = Task { [weak self] in
			guard let self else { return }
			do {
				let profiles = try await HMuxBackend.loadProfiles()
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				self.availableProfiles = profiles
				self.isLoadingProfiles = false
				self.profileTask = nil
			} catch {
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				Self.logger.error("Session profiles failed to load")
				self.profileLoadErrorMessage = error.localizedDescription
				self.isLoadingProfiles = false
				self.profileTask = nil
			}
		}
	}

	func createSession(
		profileID: String,
		name: String,
		completion: @escaping (Bool, String?) -> Void
	) {
		let normalizedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
		guard !isCreatingSession,
		      workspaceSourceKey != nil,
		      availableProfiles.contains(where: { $0.id == profileID }),
		      normalizedName.isEmpty || hmuxIsValidSessionName(normalizedName) else {
			completion(false, "Choose an available profile and use a valid optional session name.")
			return
		}
		isCreatingSession = true
		let generation = lifecycleGeneration
		createSessionTask = Task { [weak self] in
			guard let self else { return }
			do {
				let creation: HMuxSessionCreation
				if let pending = self.pendingCreatedSession, pending.profileID == profileID, pending.name == normalizedName {
					creation = pending.creation
				} else {
					self.pendingCreatedSession = nil
					self.hasCreatedSessionToOpen = false
					creation = try await HMuxBackend.createSession(profileID: profileID, name: normalizedName)
					guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
					self.pendingCreatedSession = (profileID, normalizedName, creation)
					self.hasCreatedSessionToOpen = true
				}
				let startingRevision = self.acceptedCatalogRevision
				let catalog = try await HMuxBackend.loadCatalog()
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				guard catalog.workspaceSourceKey == self.workspaceSourceKey else {
					throw HMuxBackendError.backend("The Home connection settings changed. Reopen HMux before continuing.")
				}
				guard let session = catalog.sessions.first(where: {
					$0.id == creation.session.id && $0.createdAt == creation.session.createdAt
				}) else {
					throw HMuxBackendError.backend("The session was created but did not appear in the Home catalog yet. Try opening it from the catalog in a moment.")
				}
				// Preserve a newer streamed snapshot received during this lookup.
				if self.acceptedCatalogRevision == startingRevision {
					_ = self.acceptCatalogProjection(catalog)
				}
				guard self.open(session) else {
					throw HMuxBackendError.backend("The session was created, but its terminal could not be opened.")
				}
				self.isCreatingSession = false
				self.pendingCreatedSession = nil
				self.hasCreatedSessionToOpen = false
				self.createSessionTask = nil
				self.actionErrorMessage = nil
				completion(true, nil)
			} catch {
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				Self.logger.error("Session creation failed")
				self.isCreatingSession = false
				self.createSessionTask = nil
				completion(false, error.localizedDescription)
				self.requestCatalogRefreshIfNeeded()
			}
		}
	}

	func reconnectSelectedTab() {
		guard let selectedTab else { return }
		reconnect(selectedTab)
	}

	func reconnect(_ tab: HMuxTerminalTab) {
		guard workspaceSourceKey != nil, !hasPresentedModal else { return }
		guard let index = tabs.firstIndex(where: { $0 === tab }) else { return }
		selectionEpoch &+= 1
		tab.fileTransferTask?.cancel()
		tab.fileTransferTask = nil
		tab.fileTransferState = .idle
		guard let currentSession = hmuxRestoredSession(HMuxSessionIdentity(session: tab.session), in: Array(latestSessions.values)) ?? (tab.isMissing ? nil : tab.session) else {
			actionErrorMessage = "This tmux session no longer exists in the Home catalog. Close this visual tab when ready."
			return
		}
		do {
			let replacement = try HMuxTerminalTab(session: currentSession, ghostty: ghostty)
			var nextTabs = tabs
			nextTabs[index] = replacement
			tabs = nextTabs
			if selectedTabID == tab.id {
				selectedTabID = replacement.id
				selectedCatalogID = replacement.id
				requestTerminalFocus()
			}
			updateSurfaceVisibility()
            persistWorkspace()
			actionErrorMessage = nil
		} catch {
			Self.logger.error("Terminal surface reconnection failed")
			actionErrorMessage = "HMux could not reconnect this visual tab: \(error.localizedDescription)"
		}
	}

    private func retryRecoveredTabs() {
        guard recoveryRetryTask == nil else { return }
        let pending = tabs.filter { $0.isMissing && hmuxRestoredSession(HMuxSessionIdentity(session: $0.session), in: Array(latestSessions.values)) != nil }
        guard !pending.isEmpty else { return }
        let generation = lifecycleGeneration
        recoveryRetryTask = Task { [weak self] in
            defer { if self?.lifecycleGeneration == generation { self?.recoveryRetryTask = nil } }
            while !Task.isCancelled {
                do {
                    guard let self, self.lifecycleGeneration == generation else { return }
                    let pending = self.tabs.filter { $0.isMissing && hmuxRestoredSession(HMuxSessionIdentity(session: $0.session), in: Array(self.latestSessions.values)) != nil }
                    guard !pending.isEmpty else { return }
                    for tab in pending { self.reconnect(tab) }
                }
                do { try await Task.sleep(nanoseconds: 2_000_000_000) } catch { return }
            }
        }
    }

	func pasteReadyFiles(from tab: HMuxTerminalTab) {
		guard case .ready(let transfer) = tab.fileTransferState else { return }
		let surfaceModel = tab.surfaceView.surfaceModel
		guard hmuxCanPasteReadyFileStage(
			targetMatches: exactFileTransferTarget(tab: tab, session: transfer.session, surfaceID: transfer.surfaceID),
			tabSelected: selectedTab === tab,
			appActive: NSApp.isActive,
			keyWindow: tab.surfaceView.window?.isKeyWindow == true,
			hasSurfaceModel: surfaceModel != nil
		), let surfaceModel,
		      let pasteText = try? hmuxFileStagePasteText(transfer.result) else {
			return
		}
		surfaceModel.sendText(pasteText)
		tab.fileTransferState = .idle
	}

	func dismissFileTransfer(from tab: HMuxTerminalTab) {
		tab.fileTransferTask?.cancel()
		tab.fileTransferTask = nil
		tab.fileTransferState = .idle
	}

    func restartForUpdate() {
		persistWorkspace()
		guard appUpdate != nil, restartTask == nil else { return }
		let generation = lifecycleGeneration
		restartTask = Task { [weak self] in
			guard let self else { return }
			do {
				try await self.launchReplacementAndWaitForReadiness()
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				NSApplication.shared.terminate(nil)
			} catch {
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				self.restartTask = nil
				self.actionErrorMessage = "HMux kept this window open because restart failed: \(error.localizedDescription) Quit and reopen the app when convenient."
			}
		}
    }

	private func launchReplacementAndWaitForReadiness() async throws {
		guard let bundleIdentifier = Bundle.main.bundleIdentifier else {
			throw HMuxRestartError.launcherFailed
		}
		let readinessRequest: HMuxRestartReadinessRequest
		do {
			readinessRequest = try HMuxRestartReadiness.makeRequest()
		} catch {
			throw HMuxRestartError.launcherFailed
		}
		defer { HMuxRestartReadiness.cleanup(readinessRequest) }
		let bundleURL = Bundle.main.bundleURL.standardizedFileURL
		let previousPIDs = Set(NSWorkspace.shared.runningApplications.compactMap { application -> pid_t? in
			guard application.bundleIdentifier == bundleIdentifier,
			      application.bundleURL?.standardizedFileURL == bundleURL else { return nil }
			return application.processIdentifier
		})
		let launcher = Process()
		launcher.executableURL = URL(fileURLWithPath: "/usr/bin/open")
		launcher.arguments = ["-n"] + readinessRequest.launchArguments + [bundleURL.path]
		launcher.standardInput = FileHandle.nullDevice
		launcher.standardOutput = FileHandle.nullDevice
		launcher.standardError = FileHandle.nullDevice
		try launcher.run()
		let launcherDeadline = Date().addingTimeInterval(5)
		while launcher.isRunning, Date() < launcherDeadline {
			try Task.checkCancellation()
			try await Task.sleep(nanoseconds: 50_000_000)
		}
		guard !launcher.isRunning else {
			launcher.terminate()
			throw HMuxRestartError.launcherFailed
		}
		launcher.waitUntilExit()
		guard launcher.terminationReason == .exit, launcher.terminationStatus == 0 else {
			throw HMuxRestartError.launcherFailed
		}

		let readinessDeadline = Date().addingTimeInterval(15)
		var candidate: NSRunningApplication?
		while Date() < readinessDeadline {
			try Task.checkCancellation()
			if let readyPID = HMuxRestartReadiness.readyPID(for: readinessRequest),
			   !previousPIDs.contains(readyPID) {
				candidate = NSWorkspace.shared.runningApplications.first { application in
					application.processIdentifier == readyPID &&
					application.bundleIdentifier == bundleIdentifier &&
					application.bundleURL?.standardizedFileURL == bundleURL &&
					!application.isTerminated
				}
			}
			if candidate != nil { break }
			try await Task.sleep(nanoseconds: 100_000_000)
		}
		guard let candidate else { throw HMuxRestartError.replacementNotReady }
		let stabilityDeadline = Date().addingTimeInterval(500.0 / 1000.0)
		while Date() < stabilityDeadline {
			try Task.checkCancellation()
			guard !candidate.isTerminated else { throw HMuxRestartError.replacementNotReady }
			try await Task.sleep(nanoseconds: 100_000_000)
		}
	}

    func beginAliasEdit(_ session: HMuxSession) {
		guard !hasPresentedModal, workspaceSourceKey != nil else { return }
        aliasEditorSession = latestSessions[session.identity] ?? session
    }

    func beginTermination(_ session: HMuxSession) {
		guard !hasPresentedModal, workspaceSourceKey != nil else { return }
        terminationSession = latestSessions[session.identity] ?? session
    }

    func setAlias(
        _ alias: String,
        for session: HMuxSession,
        completion: ((Bool, String?) -> Void)? = nil
    ) {
		guard requireWorkspaceBinding(completion: completion) else { return }
		let identity = session.identity
		guard pendingMutationIDs.insert(identity).inserted else {
			completion?(false, "Another update for this session is already in progress.")
			return
		}

		mutationToken &+= 1
		let token = mutationToken
		let desiredAlias = hmuxCanonicalAlias(alias)
		let previousOverlay = aliasOverlays[identity]
		let previousAlias = hmuxCanonicalAlias((latestSessions[identity] ?? session).alias)
		aliasConfirmationTasks.removeValue(forKey: identity)?.cancel()
		aliasOverlays[identity] = HMuxMutationFence(
			token: token,
			previous: previousAlias,
			desired: desiredAlias
		)
		publishAliasProjection(desiredAlias, identity: identity)

		let generation = lifecycleGeneration
		let task = Task { [weak self] in
			guard let self else { return }
			do {
				try await HMuxBackend.setAlias(alias, for: session)
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				self.mutationTasks.removeValue(forKey: identity)
				pendingMutationIDs.remove(identity)
				if var overlay = aliasOverlays[identity], overlay.token == token {
					overlay.isAcknowledged = true
					aliasOverlays[identity] = overlay
					beginAliasConfirmation(identity: identity, overlay: overlay)
				}
				// Presentation owns an immutable opening snapshot. Complete the
				// sheet from the store so a SwiftUI redraw cannot strand Saving.
				if aliasEditorSession?.identity == identity { aliasEditorSession = nil }
				if completion == nil { actionErrorMessage = nil }
				completion?(true, nil)
				requestCatalogRefreshIfNeeded()
			} catch {
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				Self.logger.error("Session alias mutation failed")
				self.mutationTasks.removeValue(forKey: identity)
				let message = error.localizedDescription
				pendingMutationIDs.remove(identity)
				if aliasOverlays[identity]?.token == token {
					aliasConfirmationTasks.removeValue(forKey: identity)?.cancel()
					if let previousOverlay {
						aliasOverlays[identity] = previousOverlay
						publishAliasProjection(previousOverlay.desired, identity: identity)
						if previousOverlay.isAcknowledged {
							beginAliasConfirmation(identity: identity, overlay: previousOverlay)
						}
					} else {
						aliasOverlays.removeValue(forKey: identity)
						let rollbackAlias = catalogSessions[identity].map { hmuxCanonicalAlias($0.alias) } ?? previousAlias
						publishAliasProjection(rollbackAlias, identity: identity)
					}
				}
				if completion == nil { actionErrorMessage = message }
				completion?(false, message)
				requestCatalogRefreshIfNeeded()
			}
		}
		mutationTasks[identity] = task
    }

    func setHidden(
        _ hidden: Bool,
        for session: HMuxSession,
        completion: ((Bool, String?) -> Void)? = nil
    ) {
		guard requireWorkspaceBinding(completion: completion) else { return }
		let identity = session.identity
		guard pendingMutationIDs.insert(identity).inserted else {
			completion?(false, "Another update for this session is already in progress.")
			return
		}

		mutationToken &+= 1
		let token = mutationToken
		let previousOverlay = hiddenOverlays[identity]
		let previousHidden = (latestSessions[identity] ?? session).isHidden
		hiddenConfirmationTasks.removeValue(forKey: identity)?.cancel()
		hiddenOverlays[identity] = HMuxMutationFence(
			token: token,
			previous: previousHidden,
			desired: hidden
		)
		publishHiddenProjection(hidden, identity: identity)

		let generation = lifecycleGeneration
		let task = Task { [weak self] in
			guard let self else { return }
			do {
				try await HMuxBackend.setHidden(hidden, for: session)
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				self.mutationTasks.removeValue(forKey: identity)
				pendingMutationIDs.remove(identity)
				if var overlay = hiddenOverlays[identity], overlay.token == token {
					overlay.isAcknowledged = true
					hiddenOverlays[identity] = overlay
					beginHiddenConfirmation(identity: identity, overlay: overlay)
				}
				if completion == nil { actionErrorMessage = nil }
				completion?(true, nil)
				requestCatalogRefreshIfNeeded()
			} catch {
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				Self.logger.error("Session visibility mutation failed")
				self.mutationTasks.removeValue(forKey: identity)
				let message = error.localizedDescription
				pendingMutationIDs.remove(identity)
				if hiddenOverlays[identity]?.token == token {
					hiddenConfirmationTasks.removeValue(forKey: identity)?.cancel()
					if let previousOverlay {
						hiddenOverlays[identity] = previousOverlay
						publishHiddenProjection(previousOverlay.desired, identity: identity)
						if previousOverlay.isAcknowledged {
							beginHiddenConfirmation(identity: identity, overlay: previousOverlay)
						}
					} else {
						hiddenOverlays.removeValue(forKey: identity)
						publishHiddenProjection(catalogSessions[identity]?.isHidden ?? previousHidden, identity: identity)
					}
				}
				if completion == nil { actionErrorMessage = message }
				completion?(false, message)
				requestCatalogRefreshIfNeeded()
			}
		}
		mutationTasks[identity] = task
    }

	func terminate(_ session: HMuxSession, completion: ((Bool, String?) -> Void)? = nil) {
		guard requireWorkspaceBinding(completion: completion) else { return }
		let identity = session.identity
		guard pendingMutationIDs.insert(identity).inserted else {
			completion?(false, "Another update for this session is already in progress.")
			return
		}
		let generation = lifecycleGeneration
		let task = Task { [weak self] in
			guard let self else { return }
            do {
				try await HMuxBackend.terminate(session)
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
				self.mutationTasks.removeValue(forKey: identity)
				close(id: identity, remember: false)
				pendingMutationIDs.remove(identity)
                if completion == nil { actionErrorMessage = nil }
                completion?(true, nil)
				requestCatalogRefreshIfNeeded()
            } catch {
				guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
                Self.logger.error("Session termination failed")
				self.mutationTasks.removeValue(forKey: identity)
                let message = error.localizedDescription
				pendingMutationIDs.remove(identity)
                if completion == nil { actionErrorMessage = message }
                completion?(false, message)
				requestCatalogRefreshIfNeeded()
            }
        }
		mutationTasks[identity] = task
    }

    func isMutationPending(for session: HMuxSession) -> Bool {
        pendingMutationIDs.contains(session.identity)
    }

    func close(_ tab: HMuxTerminalTab) {
        close(id: tab.id)
    }

    private func connectionDidClose(surface: Ghostty.SurfaceView) {
        guard let tab = tabs.first(where: { $0.surfaceView === surface }) else { return }
		// A dropped SSH connection is recoverable. Keep the visual tab and
		// its scrollback until the user reconnects or explicitly closes it.
		tab.isConnectionClosed = true
		if tab.fileTransferState != .idle {
			tab.fileTransferTask?.cancel()
			tab.fileTransferTask = nil
			tab.fileTransferState = .failed(operationID: nil, message: "The connection closed. Reconnect before dropping files again.")
		}
    }

	private func close(id: String, remember: Bool = true) {
        guard let index = tabs.firstIndex(where: { $0.id == id }) else { return }
		if remember && !tabs[index].isMissing && latestSessions[id] != nil {
			recentlyClosedTabs.removeAll { $0.identity == id }
			recentlyClosedTabs.append(HMuxClosedTab(identity: id, index: index))
			recentlyClosedTabs = Array(recentlyClosedTabs.suffix(10))
		}
		tabs[index].fileTransferTask?.cancel()
		tabs[index].fileTransferTask = nil
        tabs.remove(at: index)
		recentTabIDs.removeAll { $0 == id }
        if selectedTabID == id {
			let nextID = hmuxTabAfterClosing(index: index, remaining: tabs.map(\.id), recent: recentTabIDs)
			if let tab = tabs.first(where: { $0.id == nextID }) {
				select(tab)
            } else {
				selectionEpoch &+= 1
                selectedTabID = nil
                selectedCatalogID = nil
            }
        } else if selectedCatalogID == id {
            selectedCatalogID = selectedTabID
        }
		focusSelectedTerminal()
		persistWorkspace()
    }

	private func beginFileDrop(operationID: UUID, urls: [URL], surface: Ghostty.SurfaceView) {
		guard requireWorkspaceBinding() else { return }
		guard let tab = tabs.first(where: { $0.surfaceView === surface }) else { return }
		guard !tab.isConnectionClosed, !surface.processExited else {
			tab.fileTransferState = .failed(operationID: operationID, message: "Reconnect this tab before dropping files.")
			return
		}
		guard (1...16).contains(urls.count), urls.allSatisfy(\.isFileURL) else {
			tab.fileTransferTask?.cancel()
			tab.fileTransferTask = nil
			tab.fileTransferState = .failed(
				operationID: operationID,
				message: "Drop between 1 and 16 regular files. Folders and filesystem links are not uploaded."
			)
			return
		}
		tab.fileTransferTask?.cancel()
		let session = HMuxSessionIdentity(session: tab.session)
		let surfaceID = surface.id
		let capturedEpoch = selectionEpoch
		let requestID = hmuxFileStageRequestID(operationID)
		tab.fileTransferState = .staging(operationID: operationID, fileCount: urls.count)
		tab.fileTransferTask = Task { [weak self, weak tab, weak surface] in
			do {
				let result = try await HMuxBackend.stageFiles(urls, for: session, requestID: requestID)
				try Task.checkCancellation()
				guard let self, let tab, let surface,
				      tab.fileTransferState.operationID == operationID else { return }
				tab.fileTransferTask = nil
				let ready = HMuxReadyFileTransfer(
					operationID: operationID,
					session: session,
					surfaceID: surfaceID,
					result: result
				)
				guard let pasteText = try? hmuxFileStagePasteText(result) else {
					tab.fileTransferState = .failed(
						operationID: operationID,
						message: HMuxBackendError.invalidFileStage.localizedDescription
					)
					return
				}
				if self.canAutoPasteFileTransfer(
					tab: tab,
					surface: surface,
					session: session,
					surfaceID: surfaceID,
					capturedEpoch: capturedEpoch
				), let surfaceModel = surface.surfaceModel {
					surfaceModel.sendText(pasteText)
					tab.fileTransferState = .idle
				} else {
					tab.fileTransferState = .ready(ready)
				}
			} catch {
				guard let tab, tab.fileTransferState.operationID == operationID else { return }
				tab.fileTransferTask = nil
				if Task.isCancelled || error is CancellationError {
					tab.fileTransferState = .idle
				} else {
					tab.fileTransferState = .failed(
						operationID: operationID,
						message: error.localizedDescription
					)
				}
			}
		}
	}

	private func failFileDrop(operationID: UUID, surface: Ghostty.SurfaceView) {
		guard let tab = tabs.first(where: { $0.surfaceView === surface }) else { return }
		tab.fileTransferTask?.cancel()
		tab.fileTransferTask = nil
		tab.fileTransferState = .failed(
			operationID: operationID,
			message: "HMux could not read that file drop. Try dropping 1–16 regular files again."
		)
	}

	private func exactFileTransferTarget(
		tab: HMuxTerminalTab,
		session: HMuxSessionIdentity,
		surfaceID: UUID
	) -> Bool {
		tabs.contains(where: { $0 === tab }) &&
			tab.surfaceView.id == surfaceID &&
			tab.session.id == session.id &&
			tab.session.createdAt == session.createdAt &&
			!tab.isMissing && !tab.isConnectionClosed && !tab.surfaceView.processExited
	}

	private func canAutoPasteFileTransfer(
		tab: HMuxTerminalTab,
		surface: Ghostty.SurfaceView,
		session: HMuxSessionIdentity,
		surfaceID: UUID,
		capturedEpoch: UInt64
	) -> Bool {
		hmuxShouldAutoPasteFileStage(
			targetMatches: exactFileTransferTarget(tab: tab, session: session, surfaceID: surfaceID) &&
				tab.surfaceView === surface,
			tabSelected: selectedTab === tab,
			epochMatches: selectionEpoch == capturedEpoch,
			appActive: NSApp.isActive,
			keyWindow: surface.window?.isKeyWindow == true,
			hasSurfaceModel: surface.surfaceModel != nil
		)
	}

    private func selectTab(offset: Int) {
        guard !tabs.isEmpty else { return }
        let currentIndex = tabs.firstIndex { $0.id == selectedTabID } ?? 0
        let nextIndex = (currentIndex + offset + tabs.count) % tabs.count
        select(tabs[nextIndex])
    }

    private func matches(_ session: HMuxSession, query: String) -> Bool {
		hmuxSessionMatches(session, query: query)
    }

	private func requireWorkspaceBinding(completion: ((Bool, String?) -> Void)? = nil) -> Bool {
		guard workspaceSourceKey != nil else {
			let message = "The Home connection could not be verified yet. Wait for it to reconnect before changing a session."
			if let completion { completion(false, message) } else { actionErrorMessage = message }
			return false
		}
		return true
	}

	private func publishAliasProjection(_ alias: String?, identity: String) {
		let canonicalAlias = hmuxCanonicalAlias(alias)
		objectWillChange.send()
		if let session = latestSessions[identity] {
			latestSessions[identity] = hmuxSession(session, replacingAlias: canonicalAlias)
		}
		if let row = sessionRows.first(where: { $0.id == identity }) {
			let projected = hmuxSession(row.session, replacingAlias: canonicalAlias)
			if row.session != projected { row.session = projected }
		}
		if let tab = tabs.first(where: { $0.id == identity }) {
			let projected = hmuxSession(tab.session, replacingAlias: canonicalAlias)
			if tab.session != projected { tab.session = projected }
		}
		if let terminating = terminationSession, terminating.identity == identity {
			terminationSession = hmuxSession(terminating, replacingAlias: canonicalAlias)
		}
		resortSessionRows()
	}

	private func publishHiddenProjection(_ hidden: Bool, identity: String) {
		objectWillChange.send()
		if let session = latestSessions[identity] {
			latestSessions[identity] = hmuxSession(session, replacingHidden: hidden)
		}
		if let row = sessionRows.first(where: { $0.id == identity }) {
			let projected = hmuxSession(row.session, replacingHidden: hidden)
			if row.session != projected { row.session = projected }
		}
		if let tab = tabs.first(where: { $0.id == identity }) {
			let projected = hmuxSession(tab.session, replacingHidden: hidden)
			if tab.session != projected { tab.session = projected }
		}
		if let terminating = terminationSession, terminating.identity == identity {
			terminationSession = hmuxSession(terminating, replacingHidden: hidden)
		}
	}

	private func resortSessionRows() {
		let sorted = sessionRows.sorted { hmuxSessionCatalogLessThan($0.session, $1.session) }
		if sorted.map(\.id) != sessionRows.map(\.id) { sessionRows = sorted }
	}

	private func beginAliasConfirmation(identity: String, overlay: HMuxMutationFence<String?>) {
		aliasConfirmationTasks.removeValue(forKey: identity)?.cancel()
		let generation = lifecycleGeneration
		aliasConfirmationTasks[identity] = Task { [weak self] in
			guard let self else { return }
			var delay: UInt64 = 1_000_000_000
			while !Task.isCancelled {
				let catalog = try? await HMuxBackend.loadCatalog()
				guard !Task.isCancelled,
				      self.lifecycleGeneration == generation,
				      self.aliasOverlays[identity]?.token == overlay.token else { return }
				if let catalog, self.isNewConfirmation(catalog) {
					// This read started after the write completed. Retire only this
					// mutation and publish through the usual interaction deferral.
					self.aliasOverlays.removeValue(forKey: identity)
					self.acceptCatalogProjection(catalog)
					self.aliasConfirmationTasks.removeValue(forKey: identity)
					return
				}
				// An outage must not expose a pre-write value or leave a permanent
				// local override after three failed reads. Retry until confirmed.
				do { try await Task.sleep(nanoseconds: delay) }
				catch { return }
				delay = min(delay * 2, 30_000_000_000)
			}
		}
	}

	private func beginHiddenConfirmation(identity: String, overlay: HMuxMutationFence<Bool>) {
		hiddenConfirmationTasks.removeValue(forKey: identity)?.cancel()
		let generation = lifecycleGeneration
		hiddenConfirmationTasks[identity] = Task { [weak self] in
			guard let self else { return }
			var delay: UInt64 = 1_000_000_000
			while !Task.isCancelled {
				let catalog = try? await HMuxBackend.loadCatalog()
				guard !Task.isCancelled,
				      self.lifecycleGeneration == generation,
				      self.hiddenOverlays[identity]?.token == overlay.token else { return }
				if let catalog, self.isNewConfirmation(catalog) {
					// This read started after the write completed. Retire only this
					// mutation and publish through the usual interaction deferral.
					self.hiddenOverlays.removeValue(forKey: identity)
					self.acceptCatalogProjection(catalog)
					self.hiddenConfirmationTasks.removeValue(forKey: identity)
					return
				}
				// An outage must not expose a pre-write value or leave a permanent
				// local override after three failed reads. Retry until confirmed.
				do { try await Task.sleep(nanoseconds: delay) }
				catch { return }
				delay = min(delay * 2, 30_000_000_000)
			}
		}
	}

	private func isNewConfirmation(_ catalog: HMuxCatalog) -> Bool {
		guard catalog.workspaceSourceKey == workspaceSourceKey,
		      let stamp = HMuxCatalogStamp(catalog.generatedAt) else { return false }
		return acceptedCatalogStamp.map { stamp > $0 } ?? true
	}

	private func checkForNativeUpdate() {
		guard nativeUpdateTask == nil else { return }
		let generation = lifecycleGeneration
		nativeUpdateTask = Task { [weak self] in
			guard let self else { return }
			while self.lifecycleGeneration == generation, !Task.isCancelled {
				do {
					let status = try await HMuxBackend.checkForNativeUpdate()
					guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
					if let status { self.appUpdate = status }
				} catch {
					guard self.lifecycleGeneration == generation, !Task.isCancelled else { return }
					Self.logger.error("Automatic native app update check failed")
					self.actionErrorMessage = self.nativeUpdateFailureMessage(error)
				}
				do { try await Task.sleep(nanoseconds: 6 * 60 * 60 * 1_000_000_000) }
				catch { return }
			}
		}
	}

	private func nativeUpdateFailureMessage(_ error: Error) -> String {
		let bundlePath = Bundle.main.bundleURL.standardizedFileURL.path
		if bundlePath == "/Applications/HMux.app" || bundlePath.hasPrefix("/Applications/HMux.app/") {
			return "Safe automatic updates require a user-owned install. Move HMux.app to ~/Applications, reopen it, and try again."
		}
		return "HMux could not check for automatic updates: \(error.localizedDescription)"
	}

	func setSidebarSearchInteraction(_ active: Bool) {
        guard active != (interactionLeaseCounts[.sidebarSearch] != nil) else { return }
        if active { terminalFocusTask?.cancel(); selectedTab?.surfaceView.focusDidChange(false) }
		setInteraction(.sidebarSearch, active: active)
	}

	private func observeInteraction(
		start: Notification.Name,
		end: Notification.Name,
		lease: HMuxInteractionLease
	) {
		interactionObservers.append(NotificationCenter.default.addObserver(
			forName: start, object: nil, queue: .main
		) { [weak self] _ in
			MainActor.assumeIsolated { self?.setInteraction(lease, active: true) }
		})
		interactionObservers.append(NotificationCenter.default.addObserver(
			forName: end, object: nil, queue: .main
		) { [weak self] _ in
			MainActor.assumeIsolated { self?.setInteraction(lease, active: false) }
		})
	}

	func setQuickSwitcherInteraction(_ active: Bool) {
		setInteraction(.quickSwitcher, active: active)
	}

	func setManagementInteraction(_ identifier: String, active: Bool) {
		setInteraction(.management(identifier), active: active)
	}

	private func setInteraction(_ lease: HMuxInteractionLease, active: Bool) {
		if active {
			interactionLeaseCounts[lease, default: 0] += 1
		} else if let count = interactionLeaseCounts[lease] {
			if count <= 1 {
				interactionLeaseCounts.removeValue(forKey: lease)
			} else {
				interactionLeaseCounts[lease] = count - 1
			}
		}
		if !active { scheduleDeferredApplyAfterEvent() }
	}

	private func clearTransientInteractions() {
		interactionLeaseCounts.removeValue(forKey: .pointerTracking)
		interactionLeaseCounts.removeValue(forKey: .liveScroll)
		interactionLeaseCounts.removeValue(forKey: .menuTracking)
		scheduleDeferredApplyAfterEvent()
	}

	private func scheduleDeferredApplyAfterEvent() {
		guard interactionLeaseCounts.isEmpty, deferredSessions != nil, !deferredApplyScheduled else { return }
		deferredApplyScheduled = true
		DispatchQueue.main.async { [weak self] in
			guard let self else { return }
			self.deferredApplyScheduled = false
			guard self.interactionLeaseCounts.isEmpty, let deferredSessions = self.deferredSessions else { return }
			self.deferredSessions = nil
			_ = self.apply(deferredSessions)
		}
	}

    @discardableResult
    private func applyOrDefer(_ next: [HMuxSession]) -> Bool {
		// Hold the latest snapshot without publishing row changes while AppKit or
		// SwiftUI is tracking input. Releasing the last reference-counted lease
		// applies only the newest snapshot, so refresh can never steal an event.
		if !interactionLeaseCounts.isEmpty {
            deferredSessions = next
            return false
        }
		// A direct publication is newer than anything queued by the preceding
		// interaction. Supersede that pending value before its main-loop block
		// can run, otherwise the scheduled stale value could roll the UI back.
		deferredSessions = nil
		return apply(next)
	}

    @discardableResult
    private func apply(_ next: [HMuxSession]) -> Bool {
        // Preserve row objects and their order across polling. Updating each row
        // in place keeps ScrollView anchors and trackpad/keyboard input stable.
        let rawNextByIdentity = next.reduce(into: [String: HMuxSession]()) { sessions, session in
            // The Go bridge rejects duplicate identities. Keep the first value
            // here as a non-trapping defense if a future backend regresses.
            if sessions[session.identity] == nil { sessions[session.identity] = session }
        }
		let vanishedAliasIDs = aliasOverlays.keys.filter {
			rawNextByIdentity[$0] == nil && !pendingMutationIDs.contains($0)
		}
		for identity in vanishedAliasIDs {
			aliasOverlays.removeValue(forKey: identity)
			aliasConfirmationTasks.removeValue(forKey: identity)?.cancel()
		}
		let vanishedHiddenIDs = hiddenOverlays.keys.filter {
			rawNextByIdentity[$0] == nil && !pendingMutationIDs.contains($0)
		}
		for identity in vanishedHiddenIDs {
			hiddenOverlays.removeValue(forKey: identity)
			hiddenConfirmationTasks.removeValue(forKey: identity)?.cancel()
		}
		let nextByIdentity = rawNextByIdentity.mapValues { rawSession in
			var session = rawSession
			if let overlay = aliasOverlays[session.identity] {
				session = hmuxSession(session, replacingAlias: overlay.project(hmuxCanonicalAlias(session.alias)))
			}
			if let overlay = hiddenOverlays[session.identity] {
				session = hmuxSession(session, replacingHidden: overlay.project(session.isHidden))
			}
			return session
		}
        if nextByIdentity == latestSessions { retryRecoveredTabs(); return false }
        latestSessions = nextByIdentity

        let nextIdentities = Set(nextByIdentity.keys)
        var stableRows = sessionRows.filter { nextIdentities.contains($0.id) }
        var stableIdentities = Set(stableRows.map(\.id))
		for rawSession in next where stableIdentities.insert(rawSession.identity).inserted {
			guard let session = nextByIdentity[rawSession.identity] else { continue }
			stableRows.append(HMuxSessionRowState(session: session))
		}
		stableRows.sort {
			guard let left = nextByIdentity[$0.id], let right = nextByIdentity[$1.id] else { return $0.id < $1.id }
			return hmuxSessionCatalogLessThan(left, right)
		}
		let rowsChanged = stableRows.map(\.id) != sessionRows.map(\.id)
		let rowByIdentity = stableRows.reduce(into: [String: HMuxSessionRowState]()) { rows, row in
			if rows[row.id] == nil { rows[row.id] = row }
		}
		let visibleGroupingChanged = nextByIdentity.contains { identity, session in
			guard let row = rowByIdentity[identity] else { return false }
			return sidebarGroupingKey(row.session) != sidebarGroupingKey(session)
		}
		let hiddenProjectionChanged = isHiddenManagerPresented && nextByIdentity.contains { identity, session in
			guard let row = rowByIdentity[identity] else { return false }
			return (row.session.isHidden || session.isHidden) && row.session != session
		}
		let nextAttention = HMuxAttentionSummary(sessions: nextByIdentity.values.filter { !$0.isHidden })
		let selectedChromeChanged = selectedTabID.flatMap { identity in
			guard let current = rowByIdentity[identity]?.session else { return false }
			return nextByIdentity[identity] != current
		} ?? false
		let storeProjectionChanged = visibleGroupingChanged || hiddenProjectionChanged ||
			nextAttention != attentionSummary || selectedChromeChanged
		if !rowsChanged && storeProjectionChanged { objectWillChange.send() }
		if rowsChanged {
            sessionRows = stableRows
        }
		for (identity, session) in nextByIdentity {
			guard let row = rowByIdentity[identity],
                  row.session != session else { continue }
            row.session = session
        }
        if let selectedCatalogID, nextByIdentity[selectedCatalogID] == nil {
            self.selectedCatalogID = nil
        }

        for tab in tabs {
            if let session = latestSessions[tab.id] {
                if tab.session != session { tab.session = session }
				if tab.isMissing { tab.isMissing = false }
            } else {
				if !tab.isMissing { tab.isMissing = true }
				if tab.fileTransferState != .idle {
					tab.fileTransferTask?.cancel()
					tab.fileTransferTask = nil
					tab.fileTransferState = .failed(
						operationID: tab.fileTransferState.operationID,
						message: "This tmux session changed before the dropped files were ready."
					)
				}
            }
        }
        retryRecoveredTabs()
#if DEBUG
        runUITestAutomationIfNeeded(next)
#endif
        return true
    }

	private func sidebarGroupingKey(_ session: HMuxSession) -> String {
		if session.isHidden { return "hidden" }
		let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
		if !query.isEmpty && !matches(session, query: query) { return "excluded" }
		switch sessionFilter {
		case .active where !session.hmuxIsActive: return "excluded"
		case .attention where !session.hmuxNeedsAttention: return "excluded"
		default: break
		}
		if session.hmuxNeedsAttention { return "attention" }
		return session.hmuxIsActive ? "active" : "detached"
	}

#if DEBUG
    private func runUITestAutomationIfNeeded(_ sessions: [HMuxSession]) {
        guard let identity = uiTestIdentity,
              tabs.isEmpty,
              let session = sessions.first(where: { $0.identity == identity }) else { return }
        uiTestIdentity = nil
        Self.logger.info("UI test automation opening cataloged session")
        open(session)
        Self.logger.info("UI test automation opened \(self.tabs.count, privacy: .public) visual tabs")
        guard let delay = uiTestCloseDelayMilliseconds, let tabID = selectedTabID else { return }
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: delay * 1_000_000)
            guard let self, let tab = self.tabs.first(where: { $0.id == tabID }) else { return }
            self.close(tab)
            Self.logger.info("UI test automation closed its visual tab")
        }
    }
#endif
}

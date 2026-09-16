import Combine
import Foundation
import os

enum SmokeError: LocalizedError {
    case backend(String)
    case assertion(String)

    var errorDescription: String? {
        switch self {
        case .backend(let message), .assertion(let message): return message
        }
    }
}

@MainActor
enum HMuxBackend {
    enum MutationStep {
        case success
        case failure(String)
        case waitForSuccess(String)
    }

    static var aliasSteps: [MutationStep] = []
    static var hiddenSteps: [MutationStep] = []
    static var catalogResults: [Result<HMuxCatalog, Error>] = []
    static var releasedMutationKeys = Set<String>()
    static var catalogReadCount = 0

    static func reset() {
        aliasSteps = []
        hiddenSteps = []
        catalogResults = []
        releasedMutationKeys = []
        catalogReadCount = 0
    }

    static func release(_ key: String) { releasedMutationKeys.insert(key) }

    static func setAlias(_ alias: String, for session: HMuxSession) async throws {
        guard !aliasSteps.isEmpty else { throw SmokeError.backend("missing alias mutation plan") }
        try await run(aliasSteps.removeFirst())
    }

    static func setHidden(_ hidden: Bool, for session: HMuxSession) async throws {
        guard !hiddenSteps.isEmpty else { throw SmokeError.backend("missing hidden mutation plan") }
        try await run(hiddenSteps.removeFirst())
    }

    static func loadCatalog() async throws -> HMuxCatalog {
        catalogReadCount += 1
        while catalogResults.isEmpty {
            try Task.checkCancellation()
            try await Task.sleep(nanoseconds: 2_000_000)
        }
        return try catalogResults.removeFirst().get()
    }

    private static func run(_ step: MutationStep) async throws {
        switch step {
        case .success:
            return
        case .failure(let message):
            throw SmokeError.backend(message)
        case .waitForSuccess(let key):
            while !releasedMutationKeys.contains(key) {
                try Task.checkCancellation()
                try await Task.sleep(nanoseconds: 2_000_000)
            }
        }
    }
}

enum HMuxFileTransferState: Equatable {
    case idle
    case failed(operationID: UUID?, message: String)

    var operationID: UUID? {
        switch self {
        case .idle: return nil
        case .failed(let operationID, _): return operationID
        }
    }
}

@MainActor
final class HMuxTerminalTab {
    let id: String
    var session: HMuxSession
    var isMissing = false
    var fileTransferState: HMuxFileTransferState = .idle
    var fileTransferTask: Task<Void, Never>?

    init(session: HMuxSession) {
        id = session.identity
        self.session = session
    }
}

@MainActor
final class HMuxSessionRowState: Identifiable {
    let id: String
    var session: HMuxSession

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

extension HMuxSession {
    var hmuxNeedsAttention: Bool {
        state == "waiting_approval" || state == "waiting_input" || state == "failed"
    }

    var hmuxIsActive: Bool {
        attachedClients > 0 || state == "working" || state == "running" || hmuxNeedsAttention
    }
}

extension HMuxCatalog {
    init(testGeneratedAt: String, sessions: [HMuxSession], source: String) {
        protocolVersion = 1
        generatedAt = testGeneratedAt
        self.sessions = sessions
        appUpdate = nil
		hostMetrics = nil
        workspaceSourceKey = source
    }
}

@MainActor
final class HMuxStore {
    private static let logger = Logger(subsystem: "dev.hmux.tests", category: "catalog-store")
    let objectWillChange = ObservableObjectPublisher()

    private var latestSessions: [String: HMuxSession] = [:]
    private var catalogSessions: [String: HMuxSession] = [:]
    private var aliasOverlays: [String: HMuxMutationFence<String?>] = [:]
    private var hiddenOverlays: [String: HMuxMutationFence<Bool>] = [:]
    private var aliasConfirmationTasks: [String: Task<Void, Never>] = [:]
    private var hiddenConfirmationTasks: [String: Task<Void, Never>] = [:]
    private var mutationTasks: [String: Task<Void, Never>] = [:]
    private var mutationToken: UInt64 = 0
    private var deferredSessions: [HMuxSession]?
    private var deferredApplyScheduled = false
    private var interactionLeaseCounts: [HMuxInteractionLease: Int] = [:]
    private var acceptedCatalogRevision: UInt64 = 0
    private var acceptedCatalogStamp: HMuxCatalogStamp?
    private var lifecycleGeneration: UInt64 = 1
    private var workspaceSourceKey: String?

    var sessionRows: [HMuxSessionRowState] = []
    var tabs: [HMuxTerminalTab] = []
    var aliasEditorSession: HMuxSession?
    var terminationSession: HMuxSession?
    var pendingMutationIDs = Set<String>()
    var actionErrorMessage: String?
    var isHiddenManagerPresented = false
    var selectedTabID: String?
    var selectedCatalogID: String?
    var searchText = ""
    var sessionFilter: HMuxSessionFilter = .all

    var attentionSummary: HMuxAttentionSummary {
        HMuxAttentionSummary(sessions: latestSessions.values.filter { !$0.isHidden })
    }

    init(source: String) { workspaceSourceKey = source }

    private func requestCatalogRefreshIfNeeded() {}
    var recoveryRequests: [String] = []
    private var recoveryRetryTask: Task<Void, Never>?
    var recoveryAllowed = false
    var recoveryAttempts = 0
    private func reconnect(_ tab: HMuxTerminalTab) {
        recoveryAttempts += 1
        guard recoveryAllowed else { return }
        recoveryRequests.append(tab.id)
        tab.isMissing = false
    }

// __EXACT_PRODUCTION_METHODS__
}

extension HMuxStore {
    @discardableResult
    func smokeAccept(_ catalog: HMuxCatalog) -> Bool { acceptCatalogProjection(catalog) }

    func smokeAlias(_ identity: String) -> String? { latestSessions[identity]?.alias }
    func smokeHidden(_ identity: String) -> Bool? { latestSessions[identity]?.isHidden }
    func smokeCatalogAlias(_ identity: String) -> String? { catalogSessions[identity]?.alias }
    func smokeCatalogHidden(_ identity: String) -> Bool? { catalogSessions[identity]?.isHidden }
    func smokeHasAliasFence(_ identity: String) -> Bool { aliasOverlays[identity] != nil }
    func smokeHasHiddenFence(_ identity: String) -> Bool { hiddenOverlays[identity] != nil }
    func smokeAliasFenceAcknowledged(_ identity: String) -> Bool { aliasOverlays[identity]?.isAcknowledged == true }
    func smokeHiddenFenceAcknowledged(_ identity: String) -> Bool { hiddenOverlays[identity]?.isAcknowledged == true }
    func smokeRowIDs() -> [String] { sessionRows.map(\.id) }
    func smokeRowObjects() -> [ObjectIdentifier] { sessionRows.map(ObjectIdentifier.init) }
    func smokeHasDeferredCatalog() -> Bool { deferredSessions != nil || deferredApplyScheduled }
    func smokeSetLease(_ active: Bool) { setQuickSwitcherInteraction(active) }

    func smokeCancel() {
        recoveryRetryTask?.cancel()
        lifecycleGeneration &+= 1
        mutationTasks.values.forEach { $0.cancel() }
        aliasConfirmationTasks.values.forEach { $0.cancel() }
        hiddenConfirmationTasks.values.forEach { $0.cancel() }
    }
}

@MainActor
private func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    guard condition() else { throw SmokeError.assertion(message) }
}

@MainActor
private func eventually(
    _ message: String,
    timeout: TimeInterval = 2,
    condition: @escaping () -> Bool
) async throws {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return }
        try await Task.sleep(nanoseconds: 5_000_000)
    }
    throw SmokeError.assertion(message)
}

@MainActor
private func mutateAlias(_ value: String, session: HMuxSession, store: HMuxStore) async -> (Bool, String?) {
    await withCheckedContinuation { continuation in
        store.setAlias(value, for: session) { ok, message in continuation.resume(returning: (ok, message)) }
    }
}

@MainActor
private func mutateHidden(_ value: Bool, session: HMuxSession, store: HMuxStore) async -> (Bool, String?) {
    await withCheckedContinuation { continuation in
        store.setHidden(value, for: session) { ok, message in continuation.resume(returning: (ok, message)) }
    }
}

private let sourceKey = String(repeating: "a", count: 64)

private func stamp(_ nanosecond: Int) -> String {
    "2026-09-08T00:00:00." + String(format: "%09d", nanosecond) + "Z"
}

private func fixture(
    id: String = "$1",
    name: String = "session",
    alias: String? = "A",
    hidden: Bool = false,
    createdAt: Int64 = 1,
    activityAt: Int64 = 1
) -> HMuxSession {
    HMuxSession(
        id: id, name: name, alias: alias, hidden: hidden,
        createdAt: createdAt, activityAt: activityAt, attachedClients: 0,
        windowCount: 1, windowNames: ["shell"], activeWindow: "shell",
        currentPath: "/tmp", currentCommand: "zsh", profile: nil,
        label: nil, tags: nil, kind: nil, runtime: nil, model: nil,
        state: nil, process: nil, workingSince: nil, workflow: nil,
        workflows: nil, hostAlias: "hmux-home", width: 80, height: 24
    )
}

private func catalog(_ index: Int, _ sessions: [HMuxSession]) -> HMuxCatalog {
    HMuxCatalog(testGeneratedAt: stamp(index), sessions: sessions, source: sourceKey)
}

@main
struct HMuxCatalogStoreSmoke {
    @MainActor
    static func main() async throws {
        try await recoveryUsesExplicitLineage()
        print("ok explicit-recovery-lineage")
        try await rapidAliasWithDeferredCatalog()
        print("ok rapid-alias-deferred")
        try await rapidVisibilityWithDeferredCatalog()
        print("ok rapid-hidden-deferred")
        try await failedSecondAliasRestoresFence()
        print("ok failed-second-restores-fence")
        try await directExternalEditStaysDeferred()
        print("ok direct-external-edit-deferred")
        try await confirmationRetriesRecover()
        print("ok confirmation-retry-recovery")
        try await oldAndEqualConfirmationsStayFenced()
        print("ok old-equal-confirmation-fence")
        try await aliasFailureRollsBackToClearedAuthoritativeValue()
        print("ok nil-alias-failure-rollback")
        try await recycledIdentityAndStableRows()
        print("ok recycled-identity-stable-rows")
        try await aliasEditorPresentationSurvivesSaving()
        print("ok alias-editor-save-lifecycle")
        print("catalog store smoke: 10 scenarios passed")
    }

    @MainActor
    private static func recoveryUsesExplicitLineage() async throws {
        let base = fixture()
        let store = HMuxStore(source: sourceKey)
        _ = store.smokeAccept(catalog(1, [base]))
        store.tabs = [HMuxTerminalTab(session: base)]
        var replacement = fixture(createdAt: base.createdAt + 10)
        _ = store.smokeAccept(catalog(2, [replacement]))
        try expect(store.recoveryRequests.isEmpty, "recycled tmux ID reconnected")
        replacement.restoredFrom = HMuxSessionIdentity(session: base)
        _ = store.smokeAccept(catalog(3, [replacement]))
        try await eventually("initial recovery not attempted") { store.recoveryAttempts == 1 }
        try expect(store.recoveryRequests.isEmpty, "blocked recovery replaced tab")
        store.recoveryAllowed = true
        // No further catalog publication: retry must outlive the equal-payload gate.
        try await eventually("blocked recovery never retried", timeout: 4) { store.recoveryRequests == [base.identity] }
        store.smokeCancel()
    }

    @MainActor
    private static func aliasEditorPresentationSurvivesSaving() async throws {
        HMuxBackend.reset()
        let base = fixture()
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(80, [base])), "initial editor catalog rejected")
        store.aliasEditorSession = base
        store.smokeSetLease(true)
        HMuxBackend.aliasSteps = [.waitForSuccess("editor-save")]
        let save = Task { await mutateAlias("B", session: base, store: store) }
        try await eventually("editor save did not begin") { store.pendingMutationIDs.contains(base.identity) }
        try expect(store.smokeAlias(base.identity) == "B", "optimistic label not updated")
        try expect(store.aliasEditorSession == base, "saving replaced the sheet presentation payload")
        try expect(store.smokeAccept(catalog(81, [hmuxSession(base, replacingAlias: "B")])), "stream echo rejected")
        try expect(store.aliasEditorSession == base, "stream echo replaced sheet payload")
        HMuxBackend.release("editor-save")
        let result = await save.value
        try expect(result.0 && !store.pendingMutationIDs.contains(base.identity), "save did not clear busy state")
        try expect(store.aliasEditorSession == nil, "successful write did not dismiss editor")
        // Confirmation is deliberately still unavailable. Saving must already end.
        try expect(store.smokeHasAliasFence(base.identity), "expected pending confirmation")
        store.smokeSetLease(false)
        store.aliasEditorSession = base
        HMuxBackend.aliasSteps = [.failure("write failed")]
        let failed = await mutateAlias("C", session: base, store: store)
        try expect(!failed.0 && !store.pendingMutationIDs.contains(base.identity), "failed save remained busy")
        try expect(store.aliasEditorSession == base, "failed save dismissed the editor")
        store.smokeCancel()
    }

    @MainActor
    private static func rapidAliasWithDeferredCatalog() async throws {
        HMuxBackend.reset()
        let base = fixture()
        let identity = base.identity
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(1, [base])), "initial alias catalog rejected")

        HMuxBackend.aliasSteps = [.success]
        let first = await mutateAlias("B", session: base, store: store)
        try expect(first.0 && store.smokeAlias(identity) == "B", "first alias was not optimistic/acknowledged")

        HMuxBackend.aliasSteps = [.waitForSuccess("alias-second")]
        let secondTask = Task { await mutateAlias("A", session: base, store: store) }
        try await eventually("second alias did not become pending") { store.pendingMutationIDs.contains(identity) }
        store.smokeSetLease(true)
        try expect(store.smokeAccept(catalog(2, [hmuxSession(base, replacingAlias: "B")])), "deferred B rejected")
        try expect(store.smokeAlias(identity) == "A" && store.smokeCatalogAlias(identity) == "B", "deferred B escaped optimistic A")
        HMuxBackend.catalogResults = [.success(catalog(3, [base]))]
        HMuxBackend.release("alias-second")
        let second = await secondTask.value
        try expect(second.0, "second alias failed")
        try await eventually("causal A confirmation did not clear alias fence") { !store.smokeHasAliasFence(identity) }
        try expect(store.smokeAlias(identity) == "A", "confirmation changed UI during lease")
        store.smokeSetLease(false)
        try await eventually("deferred confirmed A was not applied") { !store.smokeHasDeferredCatalog() }
        try expect(store.smokeAlias(identity) == "A", "deferred B reverted confirmed A")
        store.smokeCancel()
    }

    @MainActor
    private static func rapidVisibilityWithDeferredCatalog() async throws {
        HMuxBackend.reset()
        let base = fixture(alias: nil)
        let identity = base.identity
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(10, [base])), "initial visibility catalog rejected")

        HMuxBackend.hiddenSteps = [.success]
        let hidden = await mutateHidden(true, session: base, store: store)
        try expect(hidden.0, "hide failed")
        HMuxBackend.hiddenSteps = [.waitForSuccess("hidden-second")]
        let restoreTask = Task { await mutateHidden(false, session: base, store: store) }
        try await eventually("restore did not become pending") { store.pendingMutationIDs.contains(identity) }
        store.smokeSetLease(true)
        try expect(store.smokeAccept(catalog(11, [hmuxSession(base, replacingHidden: true)])), "deferred hidden catalog rejected")
        try expect(store.smokeHidden(identity) == false && store.smokeCatalogHidden(identity) == true, "deferred hidden state escaped restore")
        HMuxBackend.catalogResults = [.success(catalog(12, [base]))]
        HMuxBackend.release("hidden-second")
        let restored = await restoreTask.value
        try expect(restored.0, "restore failed")
        try await eventually("restore confirmation did not clear fence") { !store.smokeHasHiddenFence(identity) }
        store.smokeSetLease(false)
        try await eventually("restored visibility was not applied") { !store.smokeHasDeferredCatalog() }
        try expect(store.smokeHidden(identity) == false, "deferred hide reverted confirmed restore")
        store.smokeCancel()
    }

    @MainActor
    private static func failedSecondAliasRestoresFence() async throws {
        HMuxBackend.reset()
        let base = fixture()
        let identity = base.identity
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(20, [base])), "initial catalog rejected")
        HMuxBackend.aliasSteps = [.success]
        let first = await mutateAlias("B", session: base, store: store)
        try expect(first.0, "first alias failed")
        try expect(store.smokeAliasFenceAcknowledged(identity), "first fence not acknowledged")
        HMuxBackend.aliasSteps = [.failure("expected failure")]
        let failed = await mutateAlias("C", session: base, store: store)
        try expect(!failed.0, "second alias unexpectedly succeeded")
        try expect(store.smokeAlias(identity) == "B" && store.smokeAliasFenceAcknowledged(identity), "failed edit did not restore prior B fence")
        HMuxBackend.catalogResults = [.success(catalog(21, [hmuxSession(base, replacingAlias: "B")]))]
        try await eventually("restored prior fence did not confirm") { !store.smokeHasAliasFence(identity) }
        try expect(store.smokeAlias(identity) == "B", "confirmed prior alias changed")
        store.smokeCancel()
    }

    @MainActor
    private static func directExternalEditStaysDeferred() async throws {
        HMuxBackend.reset()
        let base = fixture()
        let identity = base.identity
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(30, [base])), "initial catalog rejected")
        store.smokeSetLease(true)
        HMuxBackend.aliasSteps = [.success]
        HMuxBackend.catalogResults = [.success(catalog(31, [hmuxSession(base, replacingAlias: "C")]))]
        let mutation = await mutateAlias("B", session: base, store: store)
        try expect(mutation.0, "alias B failed")
        try await eventually("external C confirmation did not retire fence") { !store.smokeHasAliasFence(identity) }
        try expect(store.smokeAlias(identity) == "B" && store.smokeCatalogAlias(identity) == "C", "external C published during lease")
        store.smokeSetLease(false)
        try await eventually("external C did not publish after lease") { store.smokeAlias(identity) == "C" }
        store.smokeCancel()
    }

    @MainActor
    private static func confirmationRetriesRecover() async throws {
        HMuxBackend.reset()
        let base = fixture()
        let identity = base.identity
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(40, [base])), "initial catalog rejected")
        HMuxBackend.aliasSteps = [.success]
        HMuxBackend.catalogResults = [
            .failure(SmokeError.backend("one")),
            .failure(SmokeError.backend("two")),
            .failure(SmokeError.backend("three")),
            .success(catalog(41, [hmuxSession(base, replacingAlias: "C")]))
        ]
        let mutation = await mutateAlias("B", session: base, store: store)
        try expect(mutation.0, "alias write failed")
        try await eventually("fence disappeared during initial failed confirmations", timeout: 3.5) {
            HMuxBackend.catalogReadCount >= 3 && store.smokeHasAliasFence(identity)
        }
        try await eventually("confirmation did not recover after three failures", timeout: 5) {
            !store.smokeHasAliasFence(identity) && store.smokeAlias(identity) == "C"
        }
        store.smokeCancel()
    }

    @MainActor
    private static func oldAndEqualConfirmationsStayFenced() async throws {
        HMuxBackend.reset()
        let base = fixture()
        let identity = base.identity
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(50, [base])), "initial catalog rejected")
        HMuxBackend.aliasSteps = [.success]
        HMuxBackend.catalogResults = [.success(catalog(50, [hmuxSession(base, replacingAlias: "B")]))]
        let mutation = await mutateAlias("B", session: base, store: store)
        try expect(mutation.0, "alias write failed")
        try await eventually("equal confirmation was not read") { HMuxBackend.catalogReadCount >= 1 }
        try expect(store.smokeHasAliasFence(identity), "equal confirmation retired fence")
        HMuxBackend.catalogResults.append(.success(catalog(49, [hmuxSession(base, replacingAlias: "B")])))
        try await eventually("older confirmation was not read", timeout: 2) { HMuxBackend.catalogReadCount >= 2 }
        try expect(store.smokeHasAliasFence(identity), "older confirmation retired fence")
        HMuxBackend.catalogResults.append(.success(catalog(51, [hmuxSession(base, replacingAlias: "C")])))
        try await eventually("newer confirmation did not retire fence", timeout: 4) {
            !store.smokeHasAliasFence(identity) && store.smokeAlias(identity) == "C"
        }
        store.smokeCancel()
    }

    @MainActor
    private static func aliasFailureRollsBackToClearedAuthoritativeValue() async throws {
        HMuxBackend.reset()
        let base = fixture(alias: "A")
        let identity = base.identity
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(60, [base])), "initial catalog rejected")
        store.smokeSetLease(true)
        try expect(store.smokeAccept(catalog(61, [hmuxSession(base, replacingAlias: nil)])), "cleared alias catalog rejected")
        HMuxBackend.aliasSteps = [.failure("write failed")]
        let failed = await mutateAlias("B", session: base, store: store)
        try expect(!failed.0, "alias failure expected")
        try expect(store.smokeAlias(identity) == nil, "failure restored old A instead of authoritative nil")
        store.smokeSetLease(false)
        try await eventually("cleared alias was not applied after lease") { !store.smokeHasDeferredCatalog() }
        try expect(store.smokeAlias(identity) == nil, "cleared alias did not remain after lease")
        store.smokeCancel()
    }

    @MainActor
    private static func recycledIdentityAndStableRows() async throws {
        HMuxBackend.reset()
        let alpha = fixture(id: "$9", name: "alpha", alias: nil, createdAt: 98, activityAt: 5)
        let beta = fixture(id: "$2", name: "Beta", alias: nil, createdAt: 2, activityAt: 4)
        let store = HMuxStore(source: sourceKey)
        try expect(store.smokeAccept(catalog(70, [beta, alpha])), "initial catalog rejected")
        let initialIDs = store.smokeRowIDs()
        let initialObjects = store.smokeRowObjects()
        try expect(initialIDs == [alpha.identity, beta.identity], "rows are not globally A-Z")

        let alphaActivity = fixture(id: "$9", name: "alpha", alias: nil, createdAt: 98, activityAt: 999)
        let betaActivity = fixture(id: "$2", name: "Beta", alias: nil, createdAt: 2, activityAt: 1000)
        try expect(store.smokeAccept(catalog(71, [alphaActivity, betaActivity])), "activity update rejected")
        try expect(store.smokeRowIDs() == initialIDs && store.smokeRowObjects() == initialObjects, "activity update replaced/reordered stable rows")

        HMuxBackend.aliasSteps = [.success]
        let mutation = await mutateAlias("local", session: alphaActivity, store: store)
        try expect(mutation.0, "old identity alias failed")
        let replacement = fixture(id: "$9", name: "replacement", alias: nil, createdAt: 99)
        try expect(store.smokeAccept(catalog(72, [replacement, betaActivity])), "replacement catalog rejected")
        try expect(!store.smokeHasAliasFence(alpha.identity), "old identity fence leaked")
        try expect(store.smokeAlias(replacement.identity) == nil, "old alias leaked into recycled identity")
        store.smokeCancel()
    }
}

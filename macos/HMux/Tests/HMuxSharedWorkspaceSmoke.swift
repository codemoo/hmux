import Foundation

private enum Failure: Error { case offline; case assertion(String) }
@MainActor enum HMuxBackend {
    static var requests: [HMuxSharedWorkspaceChange?] = []
    static var handler: ((HMuxSharedWorkspaceChange?) async throws -> HMuxSharedWorkspace)!
    static func sharedWorkspace(change: HMuxSharedWorkspaceChange?) async throws -> HMuxSharedWorkspace {
        requests.append(change)
        return try await handler(change)
    }
}
@MainActor final class HMuxTerminalTab {
    let id: String
    var session: HMuxSession
    var isMissing = false
    init(session: HMuxSession, ghostty: Int) throws { self.id = session.identity; self.session = session }
}
@MainActor final class TestStore {
    var sharedWorkspaceTask: Task<Void,Never>?
    var sharedWorkspaceBusy = false
    var sharedWorkspaceLoaded = false
    var sharedWorkspaceDirty = false
    var sharedWorkspaceRevision: UInt64 = 0
    var sharedWorkspaceEdit: UInt64 = 0
    var sharedWorkspaceBase: [HMuxSessionIdentity] = []
    var sharedWorkspacePending: HMuxSharedWorkspaceChange?
    var sharedWorkspacePendingEdit: UInt64 = 0
    var sharedWorkspaceLastTabs: [HMuxSessionIdentity] = []
    var sharedWorkspaceLastSelected: HMuxSessionIdentity?
    var sharedWorkspaceStatus = ""
    var workspaceSourceKey: String? = "source"
    var lifecycleGeneration: UInt64 = 0
    var isCatalogConnected = true
    var workspaceRestorationComplete = false
    var isRestoringWorkspace = false
    var latestSessions: [String:HMuxSession] = [:]
    var tabs: [HMuxTerminalTab] = []
    var selectedTabID: String?
    var selectedTab: HMuxTerminalTab? { tabs.first { $0.id == selectedTabID } }
    var actionErrorMessage: String?
    let workspacePersistence = HMuxWorkspacePersistence(defaults: UserDefaults(suiteName:"dev.hmux.tests.shared-sync")!)
    var isSidebarPresented = true
    var isInspectorPresented = false
    let ghostty = 0
    func select(_ tab: HMuxTerminalTab) { selectedTabID = tab.id }
    func close(id: String, remember: Bool) { tabs.removeAll { $0.id == id }; if selectedTabID == id { selectedTabID = nil } }
    func restoreWorkspaceIfNeeded(_ sessions: [HMuxSession]) { workspaceRestorationComplete = true }
    func synchronize() async { await syncSharedWorkspace() }
    func receive(_ value: HMuxSharedWorkspace) { applySharedWorkspace(value) }
    // __EXACT_PRODUCTION_METHODS__
}

@main struct SharedWorkspaceSmoke {
    @MainActor static func main() async throws {
        let a = HMuxSessionIdentity(id:"$1",createdAt:100)
        let b = HMuxSessionIdentity(id:"$2",createdAt:101)
        let c = HMuxSessionIdentity(id:"$3",createdAt:102)
        func state(_ revision: UInt64, _ tabs:[HMuxSessionIdentity], _ selected:HMuxSessionIdentity?) -> HMuxSharedWorkspace {
            HMuxSharedWorkspace(version:1,initialized:true,revision:revision,tabs:tabs,selected:selected)
        }
        func check(_ condition: Bool, _ message:String) throws { if !condition { throw Failure.assertion(message) } }
        let store = TestStore()
        store.receive(state(1,[a,b],a))
        try check(store.selectedTabID == "$1:100", "first selection")
        store.receive(state(2,[b,a,c],b))
        try check(store.tabs.map(\.id) == ["$2:101","$1:100","$3:102"], "shared order/open")
        try check(store.selectedTabID == "$1:100", "remote selection stole focus")
        store.receive(state(3,[b,c],b))
        try check(store.selectedTabID == "$2:101" && store.tabs.count == 2, "remote close")
        try check(store.tabs.allSatisfy(\.isMissing), "missing identities retained")

        // A lost response followed by another local edit must retry the original
        // operation and then submit the newer delta, rather than dropping it.
        store.sharedWorkspaceRevision = 3
        store.sharedWorkspaceDirty = true
        store.sharedWorkspaceEdit = 1
        HMuxBackend.handler = { _ in throw Failure.offline }
        await store.synchronize()
        let pending = store.sharedWorkspacePending!
        store.tabs.removeLast()
        store.sharedWorkspaceEdit = 2
        var calls = 0
        HMuxBackend.handler = { change in
            calls += 1
            if calls == 1 {
                try check(change?.operationId == pending.operationId, "retry ID changed")
                return state(4,[b,c],b)
            }
            try check(change?.base == [b,c] && change?.tabs == [b], "new edit lost on retry ack")
            return state(5,[b],b)
        }
        await store.synchronize()
        for _ in 0..<20 { if calls >= 2 { break }; await Task.yield() }
        try check(calls == 2 && store.tabs.count == 1 && !store.sharedWorkspaceDirty, "pending edit not flushed")
        let unchangedCount = HMuxBackend.requests.count
        HMuxBackend.handler = { change in
            try check(change == nil, "read caused a feedback write")
            return state(6,[b,c],c)
        }
        await store.synchronize()
        try check(HMuxBackend.requests.count == unchangedCount + 1 && store.selectedTabID == "$2:101", "poll feedback/focus")
        store.receive(state(7,[],nil))
        try check(store.tabs.isEmpty && store.selectedTabID == nil, "shared empty layout")
        print("shared-workspace-sync-ok")
    }
}

import AppKit
import SwiftUI

private struct FixtureSurface: Identifiable {
    let id: Int
    var rendererID = UUID()
}

@MainActor
private final class FixtureStore: ObservableObject {
    @Published var surfaces = (0..<3).map { FixtureSurface(id: $0) }
    @Published var selectedID: Int? = 0
}

@MainActor
private final class Mounts {
    static var created = Set<UUID>()
    static var removed = Set<UUID>()
}

private struct NativeProbe: NSViewRepresentable {
    let id: UUID

    func makeNSView(context: Context) -> NSView {
        precondition(Mounts.created.insert(id).inserted, "A retained native surface was recreated")
        let view = NSView()
        view.identifier = NSUserInterfaceItemIdentifier(id.uuidString)
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {}

    static func dismantleNSView(_ view: NSView, coordinator: ()) {
        if let value = view.identifier?.rawValue, let id = UUID(uuidString: value) {
            Mounts.removed.insert(id)
        }
    }
}

private struct FixtureView: View {
    @ObservedObject var store: FixtureStore

    var body: some View {
        HMuxSurfaceDeck(items: store.surfaces, selectedID: store.selectedID) { item in
            NativeProbe(id: item.rendererID).id(item.rendererID)
        }
    }
}

@main
@MainActor
struct HMuxSurfaceDeckSmoke {
    static func main() {
        _ = NSApplication.shared
        let store = FixtureStore()
        let hosting = NSHostingView(rootView: FixtureView(store: store))
        hosting.frame = NSRect(x: 0, y: 0, width: 680, height: 440)
        func settle() {
            hosting.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.02))
            hosting.layoutSubtreeIfNeeded()
        }
        settle()
        precondition(Mounts.created.count == 3, "Native fixture views did not mount")
        for index in 0..<120 {
            store.selectedID = index % 3
            settle()
        }
        precondition(Mounts.created.count == 3 && Mounts.removed.isEmpty, "Switching remounted a native surface")
        store.selectedID = nil
        settle()
        precondition(Mounts.created.count == 3 && Mounts.removed.isEmpty, "Reading mode must retain terminal surfaces")
        store.selectedID = 1
        settle()
        precondition(Mounts.created.count == 3 && Mounts.removed.isEmpty, "Returning from reading must not remount")
        store.surfaces.swapAt(0, 2)
        settle()
        precondition(Mounts.created.count == 3 && Mounts.removed.isEmpty, "Reordering remounted a native surface")
        let replaced = store.surfaces[1].rendererID
        store.surfaces[1].rendererID = UUID()
        settle()
        precondition(Mounts.created.count == 4 && Mounts.removed == [replaced], "Reconnect must replace only its renderer")
        let closed = store.surfaces.removeFirst().rendererID
        settle()
        precondition(Mounts.removed == [replaced, closed], "Closing must release only the removed renderer")
        print("surface-deck-ok switches=120 initial-mounts=3 reconnects=1 closes=1")
    }
}

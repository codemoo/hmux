import AppKit
import SwiftUI

/// Keep native terminal wrappers mounted, but hide inactive hosts in AppKit too.
/// SwiftUI opacity does not isolate NSView event monitors or first responders.
struct HMuxSurfaceDeck<Item: Identifiable, Content: View>: NSViewRepresentable {
    let items: [Item]
    let selectedID: Item.ID?
    @ViewBuilder let content: (Item) -> Content
    @Environment(\.self) private var environment

    func makeNSView(context: Context) -> HMuxSurfaceDeckView {
        HMuxSurfaceDeckView()
    }

    func updateNSView(_ view: HMuxSurfaceDeckView, context: Context) {
        view.update(
            items: items.map { item in
                (AnyHashable(item.id), AnyView(content(item).environment(\.self, environment)))
            },
            selectedID: selectedID.map(AnyHashable.init)
        )
    }
}

@MainActor
final class HMuxSurfaceDeckView: NSView {
    private var hosts: [AnyHashable: NSHostingView<AnyView>] = [:]
    private var selectedID: AnyHashable?

    override var isFlipped: Bool { true }

    func update(items: [(AnyHashable, AnyView)], selectedID: AnyHashable?) {
        let identities = Set(items.map { $0.0 })
        for id in Array(hosts.keys) where !identities.contains(id) {
            if let host = hosts.removeValue(forKey: id) {
                host.rootView = AnyView(EmptyView())
                host.layoutSubtreeIfNeeded()
                host.removeFromSuperview()
            }
        }
        self.selectedID = selectedID
        for (id, content) in items {
            let host: NSHostingView<AnyView>
            if let existing = hosts[id] {
                host = existing
                host.rootView = content
            } else {
                host = NSHostingView(rootView: content)
                host.sizingOptions = []
                host.autoresizingMask = [.width, .height]
                hosts[id] = host
                addSubview(host)
            }
            host.frame = bounds
            host.isHidden = id != selectedID
            host.setAccessibilityHidden(id != selectedID)
        }
        needsLayout = true
    }

    override func layout() {
        super.layout()
        for host in hosts.values where host.frame != bounds { host.frame = bounds }
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHiddenOrHasHiddenAncestor, let selectedID, let host = hosts[selectedID],
              !host.isHidden, frame.contains(point) else { return nil }
        // NSView.hitTest takes a point in its superview's coordinate space.
        return host.hitTest(convert(point, from: superview))
    }
}

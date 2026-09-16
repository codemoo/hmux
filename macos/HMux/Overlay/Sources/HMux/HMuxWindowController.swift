import AppKit
import SwiftUI

@MainActor
final class HMuxWindowController: NSWindowController, NSWindowDelegate {
    private let store: HMuxStore

    init(_ ghostty: Ghostty.App) {
        store = HMuxStore(ghostty: ghostty)
        let root = HMuxRootView(store: store).environmentObject(ghostty)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1320, height: 800),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "HMux"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.titlebarSeparatorStyle = .none
        window.toolbarStyle = .unifiedCompact
        window.appearance = NSAppearance(named: .darkAqua)
        window.backgroundColor = HMuxTheme.nsColor(HMuxTheme.panel)
		window.acceptsMouseMovedEvents = true
        window.minSize = NSSize(width: HMuxLayout.windowMinWidth, height: HMuxLayout.windowMinHeight)
        window.center()
        window.setFrameAutosaveName("HMuxMainWindow")
        window.contentView = NSHostingView(rootView: root)
        super.init(window: window)
        window.delegate = self
        store.start()
    }

    required init?(coder: NSCoder) { nil }

	func windowDidChangeOcclusionState(_ notification: Notification) {
		store.setWindowVisible(window?.occlusionState.contains(.visible) == true)
	}

    func windowWillClose(_ notification: Notification) {
		store.stop()
        // Dropping the store's visual tabs releases their Ghostty PTYs. The Go
        // bridge never receives terminate or detach-client from this path.
    }
}

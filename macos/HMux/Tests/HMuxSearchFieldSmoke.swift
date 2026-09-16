import AppKit
import SwiftUI

@MainActor private final class SearchState: ObservableObject {
    @Published var text = ""
    @Published var focused = false
}

private struct SearchFixture: View {
    @ObservedObject var state: SearchState
    var body: some View {
        HSplitView {
            VStack {
                HMuxSearchField(placeholder: "Search sessions", text: $state.text, isFocused: $state.focused)
                    .frame(minWidth: 0, maxWidth: .infinity).frame(height: 18)
                Spacer()
            }.padding(12).frame(minWidth: 272, idealWidth: 296, maxWidth: 360)
            Color.black.frame(minWidth: 500, maxWidth: .infinity, maxHeight: .infinity)
        }.frame(width: 960, height: 640)
    }
}

@main @MainActor struct HMuxSearchFieldSmoke {
    static func main() {
        _ = NSApplication.shared
        let state = SearchState()
        let host = NSHostingView(rootView: SearchFixture(state: state))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 960, height: 640), styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.contentView = host
        func settle() {
            host.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date().addingTimeInterval(0.03))
            host.layoutSubtreeIfNeeded()
        }
        func field(_ view: NSView) -> HMuxSearchField.StableField? {
            if let result = view as? HMuxSearchField.StableField { return result }
            for child in view.subviews { if let result = field(child) { return result } }
            return nil
        }
        settle()
        guard let editor = field(host) else { fatalError("Search editor was not mounted") }
        let baseline = host.convert(editor.bounds, from: editor)
        let windowFrame = window.frame
        for query in ["", "한글 검색", String(repeating: "long search ", count: 100), ""] {
            precondition(window.makeFirstResponder(editor), "Search must accept focus")
            state.text = query
            settle()
            precondition(editor.stringValue == query, "Search binding did not update")
            precondition(editor.intrinsicContentSize.width == NSView.noIntrinsicMetric, "Editor must not resize its parent")
            precondition(host.convert(editor.bounds, from: editor) == baseline, "Search focus/text moved split geometry")
            precondition(window.frame == windowFrame, "Search resized the window")
            window.makeFirstResponder(nil)
            settle()
        }
        window.contentView = nil
        print("search-field-ok focus text Korean long-query stable-split stable-window")
    }
}

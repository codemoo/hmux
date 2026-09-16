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
private final class RequiredEnvironment: ObservableObject {}

@MainActor
private final class ProbeRegistry {
    static var mounted: [UUID: ProbeView] = [:]
    static var itemByRenderer: [UUID: Int] = [:]
    static var environmentByRenderer: [UUID: ObjectIdentifier] = [:]
    static var removed = Set<UUID>()

    static func reset() {
        mounted.removeAll()
        itemByRenderer.removeAll()
        environmentByRenderer.removeAll()
        removed.removeAll()
    }
}

@MainActor
private final class ProbeView: NSView {
    let itemID: Int
    let rendererID: UUID
    var monitorPasses = 0
    var monitorClaims = 0
    var sameWindowChecks = 0
    var hiddenMonitorPasses = 0
    var hitOwnershipChecks = 0
    var mouseEvents: [NSEvent.EventType] = []
    var keys: [String] = []
    var commandKeyUps: [String] = []
    var ghosttyFocused = true

    init(itemID: Int, rendererID: UUID) {
        self.itemID = itemID
        self.rendererID = rendererID
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier(rendererID.uuidString)
    }

    required init?(coder: NSCoder) { fatalError() }
    override var acceptsFirstResponder: Bool { true }

    // This mirrors the defensive Ghostty monitor used by the patched source:
    // every retained surface sees a window-wide mouse-down, but only the
    // visible surface under the content view's real hit-test result may act.
    func localMouseDown(_ event: NSEvent, eventWindow: NSWindow?) -> NSEvent? {
        guard let window, eventWindow === window else {
            monitorPasses += 1
            return event
        }
        sameWindowChecks += 1
        guard !isHiddenOrHasHiddenAncestor, window.attachedSheet == nil,
              let contentView = window.contentView else {
            if isHiddenOrHasHiddenAncestor { hiddenMonitorPasses += 1 }
            monitorPasses += 1
            return event
        }
        // NSView.hitTest takes a point in the receiver's superview space.
        // Converting into contentView itself breaks flipped hosting views.
        let location = contentView.superview?.convert(event.locationInWindow, from: nil)
            ?? event.locationInWindow
        guard let hit = contentView.hitTest(location),
              hit === self || hit.isDescendant(of: self) else {
            monitorPasses += 1
            return event
        }
        hitOwnershipChecks += 1
        guard window.firstResponder !== self else {
            monitorPasses += 1
            return event
        }
        precondition(window.makeFirstResponder(self), "Visible probe could not take focus")
        monitorClaims += 1
        return nil
    }

    // Mirrors Ghostty's command-key-up local monitor guard. eventWindow is
    // supplied by the fixture dispatcher because headless NSEvents cannot
    // resolve windowNumber 0 back to an NSWindow instance.
    func localKeyUp(_ event: NSEvent, eventWindow: NSWindow?) -> NSEvent? {
        guard event.modifierFlags.contains(.command) else { return event }
        guard ghosttyFocused, !isHiddenOrHasHiddenAncestor,
              let window, eventWindow === window,
              window.firstResponder === self else { return event }
        keyUp(with: event)
        return nil
    }

    override func mouseDown(with event: NSEvent) { mouseEvents.append(.leftMouseDown) }
    override func mouseDragged(with event: NSEvent) { mouseEvents.append(.leftMouseDragged) }
    override func mouseUp(with event: NSEvent) { mouseEvents.append(.leftMouseUp) }
    override func keyDown(with event: NSEvent) { keys.append(event.characters ?? "") }
    override func keyUp(with event: NSEvent) { commandKeyUps.append(event.characters ?? "") }
}

private struct NativeProbe: NSViewRepresentable {
    @EnvironmentObject private var requiredEnvironment: RequiredEnvironment
    let itemID: Int
    let rendererID: UUID

    func makeNSView(context: Context) -> ProbeView {
        precondition(ProbeRegistry.mounted[rendererID] == nil, "Renderer mounted twice")
        let view = ProbeView(itemID: itemID, rendererID: rendererID)
        ProbeRegistry.mounted[rendererID] = view
        ProbeRegistry.itemByRenderer[rendererID] = itemID
        ProbeRegistry.environmentByRenderer[rendererID] = ObjectIdentifier(requiredEnvironment)
        return view
    }

    func updateNSView(_ view: ProbeView, context: Context) {
        precondition(ObjectIdentifier(requiredEnvironment) == ProbeRegistry.environmentByRenderer[rendererID],
                     "Environment object changed during update")
    }

    static func dismantleNSView(_ view: ProbeView, coordinator: ()) {
        ProbeRegistry.removed.insert(view.rendererID)
        ProbeRegistry.mounted.removeValue(forKey: view.rendererID)
    }
}

private struct FixtureView: View {
    @ObservedObject var store: FixtureStore

    var body: some View {
        ZStack(alignment: .topLeading) {
            HMuxSurfaceDeck(items: store.surfaces, selectedID: store.selectedID) { item in
                NativeProbe(itemID: item.id, rendererID: item.rendererID)
                    .id(item.rendererID)
            }
            .frame(width: 420, height: 260)
        }
        .frame(width: 720, height: 480, alignment: .topLeading)
    }
}

@main
@MainActor
private struct HMuxSurfaceInputSmoke {
    static func main() {
        _ = NSApplication.shared
        ProbeRegistry.reset()
        let store = FixtureStore()
        let requiredEnvironment = RequiredEnvironment()
        let hosting = NSHostingView(
            rootView: FixtureView(store: store).environmentObject(requiredEnvironment)
        )
        hosting.frame = NSRect(x: 0, y: 0, width: 720, height: 480)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 480),
            styleMask: [.borderless], backing: .buffered, defer: false
        )
        window.contentView = hosting

        func settle() {
            hosting.layoutSubtreeIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.03))
            hosting.layoutSubtreeIfNeeded()
        }

        func deckView() -> HMuxSurfaceDeckView {
            func find(_ view: NSView) -> HMuxSurfaceDeckView? {
                if let deck = view as? HMuxSurfaceDeckView { return deck }
                for child in view.subviews {
                    if let found = find(child) { return found }
                }
                return nil
            }
            guard let result = find(hosting) else { preconditionFailure("Deck did not mount") }
            return result
        }

        func renderer(for itemID: Int) -> UUID {
            guard let id = store.surfaces.first(where: { $0.id == itemID })?.rendererID else {
                preconditionFailure("Missing item \(itemID)")
            }
            return id
        }

        func probe(for itemID: Int) -> ProbeView {
            let id = renderer(for: itemID)
            guard let result = ProbeRegistry.mounted[id] else {
                preconditionFailure("Missing mounted probe \(itemID)")
            }
            return result
        }

        func assertSelection(_ itemID: Int) {
            let deck = deckView()
            let selected = probe(for: itemID)
            let visible = store.surfaces.map { probe(for: $0.id) }.filter { !$0.isHiddenOrHasHiddenAncestor }
            precondition(visible.count == 1 && visible[0] === selected,
                         "Exactly the selected native probe must be visible")
            precondition(deck.subviews.filter { !$0.isHidden }.count == 1,
                         "Exactly one retained NSHostingView must be visible")

            let pointInParent = NSPoint(x: deck.frame.midX, y: deck.frame.midY)
            guard let hit = deck.hitTest(pointInParent) else {
                preconditionFailure("Selected host was not hit-testable")
            }
            precondition(hit === selected || hit.isDescendant(of: selected),
                         "Deck hitTest did not route to selected probe")

            for item in store.surfaces where item.id != itemID {
                let inactive = probe(for: item.id)
                let pointInInactiveSuperview = inactive.convert(
                    NSPoint(x: inactive.bounds.midX, y: inactive.bounds.midY), to: inactive.superview
                )
                precondition(inactive.hitTest(pointInInactiveSuperview) == nil,
                             "Hidden native probe remained directly hit-testable")
            }
        }

        func mouseEvent(_ type: NSEvent.EventType, at probe: ProbeView) -> NSEvent {
            let local = NSPoint(x: probe.bounds.midX, y: probe.bounds.midY)
            let inWindow = probe.convert(local, to: nil)
            guard let event = NSEvent.mouseEvent(
                with: type, location: inWindow, modifierFlags: [], timestamp: 0,
                windowNumber: window.windowNumber, context: nil, eventNumber: 1,
                clickCount: 1, pressure: type == .leftMouseUp ? 0 : 1
            ) else { preconditionFailure("Could not synthesize mouse event") }
            return event
        }

        func dispatchDrag(to selected: ProbeView) {
            let down = mouseEvent(.leftMouseDown, at: selected)
            let respondersBefore = store.surfaces.map { probe(for: $0.id) }
            let eventCountsBefore = Dictionary(uniqueKeysWithValues: respondersBefore.map {
                (ObjectIdentifier($0), $0.mouseEvents.count)
            })
            let claimsBefore = Dictionary(uniqueKeysWithValues: respondersBefore.map {
                (ObjectIdentifier($0), $0.monitorClaims)
            })
            let hiddenPassesBefore = Dictionary(uniqueKeysWithValues: respondersBefore.map {
                (ObjectIdentifier($0), $0.hiddenMonitorPasses)
            })
            let hitChecksBefore = Dictionary(uniqueKeysWithValues: respondersBefore.map {
                (ObjectIdentifier($0), $0.hitOwnershipChecks)
            })
            var surviving: NSEvent? = down
            for monitor in respondersBefore where surviving != nil {
                // A headless NSWindow has windowNumber == 0, so NSEvent.window
                // cannot resolve. Pass the dispatcher-resolved window explicitly;
                // the ownership guards mirror the patched Ghostty monitor.
                surviving = monitor.localMouseDown(surviving!, eventWindow: window)
            }
            precondition(surviving === down, "A retained monitor consumed the selected probe's click")
            precondition(window.firstResponder === selected, "An inactive monitor stole focus")
            selected.mouseDown(with: down)
            selected.mouseDragged(with: mouseEvent(.leftMouseDragged, at: selected))
            selected.mouseUp(with: mouseEvent(.leftMouseUp, at: selected))
            precondition(selected.mouseEvents.suffix(3).elementsEqual([.leftMouseDown, .leftMouseDragged, .leftMouseUp]),
                         "Selected probe did not receive a complete drag")
            for probe in respondersBefore where probe !== selected {
                precondition(probe.monitorClaims == claimsBefore[ObjectIdentifier(probe)] &&
                             probe.mouseEvents.count == eventCountsBefore[ObjectIdentifier(probe)],
                             "Inactive probe claimed or received pointer input")
                precondition(probe.hiddenMonitorPasses == hiddenPassesBefore[ObjectIdentifier(probe)]! + 1,
                             "Inactive monitor did not reach and pass the hidden-host guard")
            }
            precondition(selected.hitOwnershipChecks == hitChecksBefore[ObjectIdentifier(selected)]! + 1,
                         "Selected monitor did not establish content-view hit ownership")
        }

        func sendKey(_ value: String, to selected: ProbeView) {
            precondition(window.makeFirstResponder(selected), "makeFirstResponder failed")
            precondition(window.firstResponder === selected, "Actual first responder does not match selection")
            guard let event = NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                windowNumber: window.windowNumber, context: nil,
                characters: value, charactersIgnoringModifiers: value, isARepeat: false, keyCode: 0
            ) else { preconditionFailure("Could not synthesize key event") }
            window.sendEvent(event)
            precondition(selected.keys.last == value, "Key input did not reach selected probe")
        }

        func dispatchCommandKeyUp(_ value: String, to selected: ProbeView) {
            precondition(window.firstResponder === selected,
                         "Command key-up fixture requires selected first responder")
            guard let event = NSEvent.keyEvent(
                with: .keyUp, location: .zero, modifierFlags: [.command], timestamp: 0,
                windowNumber: window.windowNumber, context: nil,
                characters: value, charactersIgnoringModifiers: value,
                isARepeat: false, keyCode: 0
            ) else { preconditionFailure("Could not synthesize command key-up") }
            let selectedCount = selected.commandKeyUps.count
            for item in store.surfaces where item.id != selected.itemID {
                let inactive = probe(for: item.id)
                let count = inactive.commandKeyUps.count
                precondition(inactive.localKeyUp(event, eventWindow: window) === event,
                             "Inactive command key-up monitor consumed the event")
                precondition(inactive.commandKeyUps.count == count,
                             "Inactive command key-up monitor delivered input")
            }
            precondition(selected.localKeyUp(event, eventWindow: window) == nil,
                         "Selected command key-up monitor did not consume the event")
            precondition(selected.commandKeyUps.count == selectedCount + 1 &&
                         selected.commandKeyUps.last == value,
                         "Selected probe did not receive command key-up")
        }

        settle()
        precondition(ProbeRegistry.mounted.count == 3, "Initial native probes did not mount")
        precondition(Set(ProbeRegistry.environmentByRenderer.values) == [ObjectIdentifier(requiredEnvironment)],
                     "Environment object did not propagate through nested NSHostingViews")
        assertSelection(0)
        let initiallyHidden = probe(for: 1)
        precondition(window.makeFirstResponder(initiallyHidden),
                     "Fixture expected AppKit to accept a hidden first responder")
        precondition(window.firstResponder === initiallyHidden,
                     "Hidden-first-responder negative control was not established")
        sendKey("a", to: probe(for: 0))
        dispatchCommandKeyUp("a", to: probe(for: 0))
        dispatchDrag(to: probe(for: 0))

        let originalRenderers = Set(ProbeRegistry.mounted.keys)
        store.selectedID = 2
        settle()
        precondition(Set(ProbeRegistry.mounted.keys) == originalRenderers && ProbeRegistry.removed.isEmpty,
                     "Selection switch remounted a retained native probe")
        assertSelection(2)
        sendKey("b", to: probe(for: 2))
        dispatchCommandKeyUp("b", to: probe(for: 2))
        dispatchDrag(to: probe(for: 2))
        precondition(probe(for: 0).keys == ["a"], "Key input leaked to previous selection")

        store.surfaces.append(FixtureSurface(id: 3))
        settle()
        precondition(ProbeRegistry.mounted.count == 4, "Added native probe did not mount")
        precondition(ProbeRegistry.environmentByRenderer[renderer(for: 3)] == ObjectIdentifier(requiredEnvironment),
                     "Environment object did not reach added probe")
        assertSelection(2)
        sendKey("d", to: probe(for: 2))
        dispatchDrag(to: probe(for: 2))

        let removedRenderer = renderer(for: 1)
        store.surfaces.removeAll { $0.id == 1 }
        settle()
        precondition(ProbeRegistry.removed == [removedRenderer] && ProbeRegistry.mounted[removedRenderer] == nil,
                     "Removing an item did not dismantle exactly its probe")
        assertSelection(2)

        let replacedRenderer = renderer(for: 2)
        let selectedIndex = store.surfaces.firstIndex { $0.id == 2 }!
        store.surfaces[selectedIndex].rendererID = UUID()
        settle()
        precondition(ProbeRegistry.removed == [removedRenderer, replacedRenderer],
                     "Replacing selected content did not dismantle exactly the old renderer")
        precondition(ProbeRegistry.mounted.count == 3, "Replacement changed retained host count")
        precondition(ProbeRegistry.environmentByRenderer[renderer(for: 2)] == ObjectIdentifier(requiredEnvironment),
                     "Environment object did not reach replacement probe")
        assertSelection(2)
        sendKey("c", to: probe(for: 2))

        // Move the native deck only after reactive updates are complete, then
        // verify the override with a point in the deck's parent coordinates.
        let deck = deckView()
        deck.frame.origin = NSPoint(x: 73, y: 41)
        deck.layoutSubtreeIfNeeded()
        precondition(deck.frame.origin != .zero, "Could not establish nonzero native deck origin")
        let pointInParent = NSPoint(x: deck.frame.midX, y: deck.frame.midY)
        guard let nonzeroHit = deck.hitTest(pointInParent) else {
            preconditionFailure("Nonzero-origin deck lost selected hit testing")
        }
        let selected = probe(for: 2)
        precondition(nonzeroHit === selected || nonzeroHit.isDescendant(of: selected),
                     "Nonzero-origin deck converted hit-test coordinates incorrectly")

        print("surface-input-ok mounts=5 switches=1 add=1 remove=1 replace=1 drag-sequences=3 environment=ok hidden-monitors=ok command-keyup=ok nonzero-origin=ok")
    }
}

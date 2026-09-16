import AppKit
import SwiftUI

enum HMuxLayout {
    static let windowMinWidth: CGFloat = 960
    static let windowMinHeight: CGFloat = 640
    static let sidebarMinWidth: CGFloat = 272
    static let sidebarIdealWidth: CGFloat = 296
    static let sidebarMaxWidth: CGFloat = 360
    static let tabBarHeight: CGFloat = 36
    static let statusBarHeight: CGFloat = 32
    static let inspectorMinWidth: CGFloat = 264
    static let inspectorIdealWidth: CGFloat = 296
    static let inspectorMaxWidth: CGFloat = 360
    static let inspectorOverlayWidth: CGFloat = 304
    static let inspectorSplitThreshold: CGFloat = 920
    static let terminalVisibleMinWidth: CGFloat = 500
    static let tabMinWidth: CGFloat = 112
    static let tabMaxWidth: CGFloat = 188
    static let rowRadius: CGFloat = 7
    static let panelRadius: CGFloat = 10
}

enum HMuxSpacing {
    static let xSmall: CGFloat = 4
    static let small: CGFloat = 8
    static let medium: CGFloat = 12
    static let large: CGFloat = 16
    static let xLarge: CGFloat = 20
    static let xxLarge: CGFloat = 24
}

enum HMuxTypography {
    static let title = Font.system(size: 15, weight: .semibold)
    static let rowTitle = Font.system(size: 13, weight: .semibold)
    static let body = Font.system(size: 13)
    static let label = Font.system(size: 11, weight: .medium)
    static let caption = Font.system(size: 11)
    static let micro = Font.system(size: 10, weight: .semibold)
}

// Flexoki Dark, pinned to euandeas/omarchy-flexoki-dark-theme.
// Keep these semantic roles shared by SwiftUI and the native window backing.
enum HMuxTheme {
    static let background: UInt32 = 0x100F0F
    static let panel: UInt32 = 0x1C1B1A
    static let raised: UInt32 = 0x282726
    static let hover: UInt32 = 0x343331
    static let selection: UInt32 = 0x403E3C
    static let foreground: UInt32 = 0xCECDC3
    static let secondary: UInt32 = 0xB7B5AC
    static let muted: UInt32 = 0x9F9D96
    static let accent: UInt32 = 0x4385BE
    static let green: UInt32 = 0x879A39
    static let yellow: UInt32 = 0xD0A215
    static let red: UInt32 = 0xD14D41
    static let cyan: UInt32 = 0x3AA99F

    static func nsColor(_ rgb: UInt32) -> NSColor {
        NSColor(
            srgbRed: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1
        )
    }
}

extension Color {
    static let hmuxWindow = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.background))
    static let hmuxChrome = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.panel))
    static let hmuxSidebar = hmuxChrome
    static let hmuxSurface = hmuxWindow
    static let hmuxRaised = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.raised))
    static let hmuxHover = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.hover))
    static let hmuxTab = hmuxChrome
    static let hmuxActiveTab = hmuxRaised
    static let hmuxBorder = hmuxRaised
    static let hmuxStrongBorder = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.selection))
    static let hmuxSelectedBackground = hmuxStrongBorder
    static let hmuxPrimaryText = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.foreground))
    static let hmuxSecondaryText = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.secondary))
    static let hmuxTertiaryText = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.muted))
    static let hmuxSelection = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.accent))
    static let hmuxConnected = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.green))
    static let hmuxWaiting = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.yellow))
    static let hmuxFailure = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.red))
    static let hmuxCompleted = Color(nsColor: HMuxTheme.nsColor(HMuxTheme.cyan))
    static let hmuxOnAccent = hmuxWindow
}

extension View {
    /// Apply at each presentation boundary so sheets/popovers stay in theme.
    func hmuxTheme() -> some View {
        self
            .foregroundStyle(Color.hmuxPrimaryText)
            .tint(Color.hmuxSelection)
            .preferredColorScheme(.dark)
    }
}

extension HMuxSession {
    var hmuxStateColor: Color {
        switch state {
        case "working", "running": return .hmuxConnected
        case "waiting_approval", "waiting_input": return .hmuxWaiting
        case "failed": return .hmuxFailure
        case "completed": return .hmuxCompleted
        default: return .hmuxSecondaryText
        }
    }

    var hmuxStateSymbol: String {
        switch state {
        case "working", "running": return "play.fill"
        case "waiting_approval": return "checkmark.shield.fill"
        case "waiting_input": return "questionmark.bubble.fill"
        case "failed": return "exclamationmark.triangle.fill"
        case "completed": return "checkmark"
        default: return "circle.fill"
        }
    }

    var hmuxStateLabel: String {
        switch state {
        case "working", "running": return "Running"
        case "waiting_approval": return "Approval"
        case "waiting_input": return "Input"
        case "failed": return "Failed"
        case "completed": return "Complete"
        default: return attachedClients > 0 ? "Attached" : "Detached"
        }
    }

    var hmuxNeedsAttention: Bool {
        state == "waiting_approval" || state == "waiting_input" || state == "failed"
    }

    var hmuxIsActive: Bool {
        attachedClients > 0 || state == "working" || state == "running" || hmuxNeedsAttention
    }

    var hmuxRuntimeLabel: String {
        let candidate = runtime ?? profile ?? detailCommand
        return candidate.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "shell" : candidate
    }

    var hmuxProjectName: String {
        let trimmed = currentPath.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "No working directory" }
        return URL(fileURLWithPath: trimmed).lastPathComponent
    }

    var hmuxActivityDate: Date {
        Date(timeIntervalSince1970: TimeInterval(activityAt))
    }

    var hmuxAttentionDate: Date {
        let timestamp = workingSince.flatMap { $0 > 0 ? $0 : nil } ?? workflow?.updatedAt ?? activityAt
        return Date(timeIntervalSince1970: TimeInterval(max(timestamp, 1)))
    }
}

func hmuxStatusColor(_ status: String) -> Color {
    switch status {
    case "running", "working": return .hmuxConnected
    case "completed": return .hmuxCompleted
    case "failed": return .hmuxFailure
    case "waiting_approval", "waiting_input": return .hmuxWaiting
    default: return .hmuxSecondaryText
    }
}

func hmuxStatusSymbol(_ status: String) -> String {
    switch status {
    case "running", "working": return "play.circle.fill"
    case "completed": return "checkmark.circle.fill"
    case "failed": return "xmark.circle.fill"
    case "waiting_approval": return "checkmark.shield.fill"
    case "waiting_input": return "questionmark.bubble.fill"
    default: return "circle"
    }
}

func hmuxCompactDuration(since start: Date, until end: Date = Date()) -> String {
    let seconds = max(0, Int(end.timeIntervalSince(start)))
    if seconds < 60 { return "<1m" }
    if seconds < 3_600 { return "\(seconds / 60)m" }
    if seconds < 86_400 { return "\(seconds / 3_600)h" }
    return "\(seconds / 86_400)d"
}

func hmuxCompactDuration(startedAt: Int64, endedAt: Int64?, now: Date = Date()) -> String {
    let start = Date(timeIntervalSince1970: TimeInterval(startedAt))
    let end = endedAt.map { Date(timeIntervalSince1970: TimeInterval($0)) } ?? now
    return hmuxCompactDuration(since: start, until: end)
}

/// A single-line editor whose intrinsic width never depends on focus or text.
/// SwiftUI plain TextField can change the split view's fitting width when its
/// AppKit field editor activates. Keep sizing owned by the surrounding chrome.
struct HMuxSearchField: NSViewRepresentable {
    let placeholder: String
    @Binding var text: String
    @Binding var isFocused: Bool
    var onSubmit: () -> Void = {}
    var onCancel: () -> Void = {}

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> StableField {
        let field = StableField()
        field.isBordered = false
        field.isBezeled = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = .systemFont(ofSize: 12)
        field.textColor = HMuxTheme.nsColor(HMuxTheme.foreground)
        field.usesSingleLineMode = true
        field.cell?.isScrollable = true
        field.lineBreakMode = .byClipping
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        field.delegate = context.coordinator
        field.setAccessibilityLabel(placeholder)
        return field
    }

    func updateNSView(_ field: StableField, context: Context) {
        context.coordinator.parent = self
        field.placeholderString = placeholder
        if field.stringValue != text { field.stringValue = text }
        // Only explicit focus requests may make this editor first responder.
        // A delayed request is checked again, so tab/modal changes can cancel it.
        if isFocused, field.currentEditor() == nil {
            DispatchQueue.main.async { [weak field, weak coordinator = context.coordinator] in
                guard let field, let coordinator, coordinator.parent.isFocused,
                      let window = field.window, window.isKeyWindow,
                      !field.isHiddenOrHasHiddenAncestor, window.attachedSheet == nil else { return }
                window.makeFirstResponder(field)
            }
        }
    }

    final class StableField: NSTextField {
        override var intrinsicContentSize: NSSize { NSSize(width: NSView.noIntrinsicMetric, height: 18) }
    }

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var parent: HMuxSearchField
        init(_ parent: HMuxSearchField) { self.parent = parent }
        func controlTextDidChange(_ notification: Notification) {
            guard let field = notification.object as? NSTextField else { return }
            parent.text = field.stringValue
        }
        func controlTextDidBeginEditing(_ notification: Notification) { parent.isFocused = true }
        func controlTextDidEndEditing(_ notification: Notification) { parent.isFocused = false }
        func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
            if commandSelector == #selector(NSResponder.insertNewline(_:)) { parent.onSubmit(); return true }
            if commandSelector == #selector(NSResponder.cancelOperation(_:)) { parent.onCancel(); return true }
            return false
        }
    }
}

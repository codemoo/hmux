import AppKit
import SwiftUI

struct HMuxQuickSwitcherView: View {
	private enum DismissalReason: Equatable {
		case cancel
		case openedTab
		case newSession
		case hiddenSessions
	}

	private enum EmptyState: Equatable {
		case connecting
		case reconnecting
		case offline
		case allHidden
		case noMatches
		case noVisibleSessions
	}

    @ObservedObject var store: HMuxStore
    @State private var query = ""
    @State private var selectedID: String?
    @State private var previousFirstResponder: NSResponder?
    @State private var keyMonitor: Any?
	@State private var openError: String?
	@State private var dismissalReason: DismissalReason = .cancel
    @State private var searchFocused = false

    private var rows: [HMuxSessionRowState] { store.sessionRows(matching: query) }
	private var trimmedQuery: String { query.trimmingCharacters(in: .whitespacesAndNewlines) }

	private var emptyState: EmptyState {
		if store.isInitialLoading { return .connecting }
		if store.isInitialFailure || store.isCatalogOffline { return .offline }
		if store.isCatalogReconnecting { return .reconnecting }
		if !store.sessions.isEmpty && store.visibleSessions.isEmpty { return .allHidden }
		if !trimmedQuery.isEmpty { return .noMatches }
		return .noVisibleSessions
	}

    var body: some View {
        ZStack {
            Color.black.opacity(0.28)
                .ignoresSafeArea()
                .contentShape(Rectangle())
                .onTapGesture { dismiss() }

            VStack(spacing: 0) {
                searchHeader
                Rectangle().fill(Color.hmuxBorder).frame(height: 1)
                results
				if let openError {
					Text(openError)
						.font(HMuxTypography.caption)
						.foregroundStyle(Color.hmuxFailure)
						.fixedSize(horizontal: false, vertical: true)
						.padding(12)
				}
                Rectangle().fill(Color.hmuxBorder).frame(height: 1)
                footer
            }
            .frame(width: 540, height: 430)
            .background(Color.hmuxChrome, in: RoundedRectangle(cornerRadius: 13))
            .overlay {
                RoundedRectangle(cornerRadius: 13)
                    .stroke(Color.hmuxBorder, lineWidth: 1)
            }
            .shadow(color: .black.opacity(0.28), radius: 28, y: 12)
            .padding(HMuxSpacing.xLarge)
        }
		.onAppear {
			store.setQuickSwitcherInteraction(true)
            previousFirstResponder = NSApp.keyWindow?.firstResponder
            selectedID = rows.first(where: { $0.id == store.selectedTabID })?.id ?? rows.first?.id
            installKeyMonitor()
            searchFocused = true
        }
		.onDisappear {
			store.setQuickSwitcherInteraction(false)
            removeKeyMonitor()
            restoreFocus()
        }
        .onChange(of: query) { _ in
			openError = nil
            repairSelection()
        }
        .onChange(of: rows.map(\.id)) { _ in
            repairSelection()
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Quick switcher")
    }

    private var searchHeader: some View {
        HStack(spacing: 10) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Color.hmuxSecondaryText)
            HMuxSearchField(
                placeholder: "Jump to a session, model, project, or tag", text: $query,
                isFocused: $searchFocused, onSubmit: openSelected, onCancel: dismiss
            )
            .frame(minWidth: 0, maxWidth: .infinity)
            .frame(height: 18)
            Text("ESC")
                .font(HMuxTypography.micro.monospaced())
                .foregroundStyle(Color.hmuxTertiaryText)
                .padding(.horizontal, 6)
                .padding(.vertical, 3)
                .background(Color.hmuxRaised, in: RoundedRectangle(cornerRadius: 5))
        }
        .padding(.horizontal, 14)
        .frame(height: 50)
    }

    @ViewBuilder
    private var results: some View {
		let openTabIDs = store.openTabIDs
        if rows.isEmpty {
			emptyResults
        } else {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 3) {
                        ForEach(rows) { row in
                            HMuxQuickSwitcherRow(
                                row: row,
                                isSelected: selectedID == row.id,
								isOpen: openTabIDs.contains(row.id)
                            ) {
                                selectedID = row.id
                                openSelected()
                            }
                            .id(row.id)
                        }
                    }
                    .padding(6)
                }
                .onChange(of: selectedID) { id in
                    guard let id else { return }
                    proxy.scrollTo(id, anchor: .center)
                }
				.onAppear {
					DispatchQueue.main.async {
						guard let selectedID else { return }
						proxy.scrollTo(selectedID, anchor: .center)
					}
				}
            }
        }
    }

	@ViewBuilder
	private var emptyResults: some View {
		VStack(spacing: 8) {
			Image(systemName: emptyIcon)
				.font(.system(size: 22, weight: .light))
			Text(emptyTitle)
				.font(HMuxTypography.rowTitle)
			Text(emptyMessage)
				.font(HMuxTypography.caption)
				.multilineTextAlignment(.center)
			if emptyState == .offline {
				Text(store.catalogErrorMessage ?? "The catalog connection is unavailable.")
					.font(HMuxTypography.micro)
					.foregroundStyle(Color.hmuxFailure)
					.textSelection(.enabled)
					.fixedSize(horizontal: false, vertical: true)
					.padding(.horizontal, HMuxSpacing.xLarge)
				Button("Retry") { store.refresh() }
					.buttonStyle(.borderedProminent)
					.controlSize(.small)
			} else if emptyState == .allHidden {
				Button("Show Hidden Sessions", action: showHiddenSessions)
					.buttonStyle(.bordered)
					.controlSize(.small)
			} else if emptyState == .noMatches {
				Button("Clear Search") { query = "" }
					.buttonStyle(.bordered)
					.controlSize(.small)
			}
		}
		.foregroundStyle(Color.hmuxSecondaryText)
		.frame(maxWidth: .infinity, maxHeight: .infinity)
	}

	private var emptyIcon: String {
		switch emptyState {
		case .connecting, .reconnecting: return "arrow.triangle.2.circlepath"
		case .offline: return "bolt.horizontal.circle"
		case .allHidden: return "eye.slash"
		case .noMatches: return "magnifyingglass"
		case .noVisibleSessions: return "rectangle.stack"
		}
	}

	private var emptyTitle: String {
		switch emptyState {
		case .connecting: return "Connecting to sessions"
		case .reconnecting: return "Reconnecting to sessions"
		case .offline: return "Catalog unavailable"
		case .allHidden: return "All sessions are hidden"
		case .noMatches: return "No matching sessions"
		case .noVisibleSessions: return "No visible sessions"
		}
	}

	private var emptyMessage: String {
		switch emptyState {
		case .connecting: return "Waiting for your Home sessions."
		case .reconnecting: return "Trying to restore the catalog connection."
		case .offline: return "Retry when the Home connection is available."
		case .allHidden: return "Restore a hidden session to return it to the catalog."
		case .noMatches: return "Search by alias, model, project, runtime, or tag."
		case .noVisibleSessions: return "Create a session when the Home connection is ready."
		}
	}

    private var footer: some View {
        HStack(spacing: 10) {
            Label("Navigate", systemImage: "arrow.up.arrow.down")
			Label("Open Session", systemImage: "return")
            Spacer()
            Text(resultSummary)
                .monospacedDigit()
			Button("New Session") { beginCreation() }
				.buttonStyle(.bordered)
				.controlSize(.small)
				.disabled(!store.canCreateSession)
				.help(store.createSessionUnavailableReason ?? "New Session")
        }
        .font(HMuxTypography.micro)
        .foregroundStyle(Color.hmuxTertiaryText)
        .padding(.horizontal, 12)
        .frame(height: 32)
    }

	private var resultSummary: String {
		if store.isCatalogOffline { return "\(rows.count) saved · Offline" }
		if store.isCatalogReconnecting { return "\(rows.count) saved · Reconnecting" }
		return query.isEmpty ? "\(rows.count) sessions · A–Z" : "\(rows.count) matches · A–Z"
	}

    private func moveSelection(offset: Int) {
        guard !rows.isEmpty else { return }
        guard let current = rows.firstIndex(where: { $0.id == selectedID }) else {
            selectedID = offset < 0 ? rows.last?.id : rows.first?.id
            return
        }
        selectedID = rows[(current + offset + rows.count) % rows.count].id
    }

    private func openSelected() {
        guard let row = rows.first(where: { $0.id == selectedID }) ?? rows.first else { return }
		guard store.open(row.session) else {
			openError = store.actionErrorMessage ?? "This session could not be opened. Try again."
			store.dismissActionError()
			return
		}
		dismissalReason = .openedTab
        dismiss()
    }

    private func repairSelection() {
        if !rows.contains(where: { $0.id == selectedID }) {
            selectedID = rows.first?.id
        }
    }

    private func dismiss() {
        store.isQuickSwitcherPresented = false
    }

	private func beginCreation() {
		dismissalReason = .newSession
		dismiss()
		DispatchQueue.main.async {
			store.beginSessionCreation()
		}
	}

	private func showHiddenSessions() {
		dismissalReason = .hiddenSessions
		dismiss()
		DispatchQueue.main.async {
			store.presentHiddenSessions()
		}
	}

    private func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            guard store.isQuickSwitcherPresented else { return event }
            let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
			let actionModifiers = modifiers.subtracting([.capsLock, .numericPad])
			if actionModifiers == .command, event.charactersIgnoringModifiers?.lowercased() == "w" {
                dismiss()
                return nil
            }
			guard actionModifiers.isEmpty else { return event }
			guard !hasMarkedText() else { return event }
            switch event.keyCode {
            case 53:
                dismiss()
                return nil
            case 126:
                moveSelection(offset: -1)
                return nil
            case 125:
                moveSelection(offset: 1)
                return nil
            case 36, 76:
                openSelected()
                return nil
            default:
                return event
            }
        }
    }

	private func hasMarkedText() -> Bool {
		guard let inputClient = NSApp.keyWindow?.firstResponder as? NSTextInputClient else { return false }
		return inputClient.hasMarkedText()
	}

    private func removeKeyMonitor() {
        guard let keyMonitor else { return }
        NSEvent.removeMonitor(keyMonitor)
        self.keyMonitor = nil
    }

    private func restoreFocus() {
        let previous = previousFirstResponder
		let reason = dismissalReason
        DispatchQueue.main.async {
			guard reason != .newSession, reason != .hiddenSessions, !store.hasPresentedModal, NSApp.isActive,
			      let window = NSApp.keyWindow, window.attachedSheet == nil else { return }
			if reason == .openedTab {
				store.focusSelectedTerminal()
				return
			}
            if let view = previous as? NSView, view.window === window, window.makeFirstResponder(view) {
                return
            }
			store.focusSelectedTerminal()
        }
    }
}

private struct HMuxQuickSwitcherRow: View {
    @ObservedObject var row: HMuxSessionRowState
    let isSelected: Bool
    let isOpen: Bool
    let action: () -> Void

    private var session: HMuxSession { row.session }

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: session.hmuxStateSymbol)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(session.hmuxStateColor)
                    .frame(width: 18)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(session.displayName)
                            .font(HMuxTypography.rowTitle)
                            .foregroundStyle(Color.hmuxPrimaryText)
							.lineLimit(1)
							.truncationMode(.tail)
							.layoutPriority(1)
                        if isOpen {
                            Text("OPEN")
                                .font(HMuxTypography.micro)
                                .foregroundStyle(Color.hmuxSelection)
                        }
                    }
                    HStack(spacing: 5) {
                        Text(session.hmuxRuntimeLabel)
                        if let model = session.modelLabel {
                            Text("·")
                            Text(model)
                        }
                        Text("·")
                        Text(session.hmuxProjectName)
                    }
                    .font(HMuxTypography.label)
                    .foregroundStyle(Color.hmuxSecondaryText)
                    .lineLimit(1)
                }
                Spacer()
                if session.hmuxNeedsAttention {
                    Text(session.hmuxStateLabel)
                        .font(HMuxTypography.micro)
                        .foregroundStyle(session.hmuxStateColor)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 4)
                        .background(session.hmuxStateColor.opacity(0.12), in: Capsule())
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 47)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(isSelected ? Color.hmuxSelectedBackground : Color.clear, in: RoundedRectangle(cornerRadius: 8))
        .accessibilityLabel("\(session.displayName), \(session.hmuxStateLabel), \(session.hmuxRuntimeLabel)")
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}

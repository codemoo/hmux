import SwiftUI

struct HMuxWorkspaceView: View {
    @ObservedObject var store: HMuxStore
	@Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 0) {
            if store.tabs.isEmpty {
                HMuxEmptyWorkspace(store: store)
            } else if let tab = store.selectedTab {
                workspace(tab: tab)
            }

            Rectangle().fill(Color.hmuxBorder).frame(height: 1)
            HMuxWorkspaceFooter(store: store)
        }
        .background(Color.hmuxSurface)
        .overlay {
            Group {
            Button("Reconnect selected tab") { store.reconnectSelectedTab() }
                .keyboardShortcut("r", modifiers: [.command, .shift])
                .disabled(store.selectedTab == nil || store.selectedTab?.isMissing == true || store.hasPresentedModal)
            Button("Close selected tab") { store.closeSelectedTab() }
                .keyboardShortcut("w", modifiers: .command)
                .disabled(store.hasPresentedModal)
            }
                .frame(width: 0, height: 0)
                .opacity(0)
                .accessibilityHidden(true)
        }
    }

    private func workspace(tab: HMuxTerminalTab) -> some View {
        GeometryReader { geometry in
            let usesOverlayInspector = geometry.size.width < HMuxLayout.inspectorSplitThreshold
            let usesBottomInspector = geometry.size.width < HMuxLayout.terminalVisibleMinWidth + HMuxLayout.inspectorMinWidth
            let overlayInspectorWidth = min(
                HMuxLayout.inspectorOverlayWidth,
                max(0, geometry.size.width - HMuxLayout.terminalVisibleMinWidth)
            )
            ZStack(alignment: usesBottomInspector ? .bottom : .trailing) {
                HSplitView {
                    HMuxSurfaceDeck(items: store.tabs, selectedID: store.isConversationPresented ? nil : store.selectedTabID) { surfaceTab in
                        HMuxTerminalSurface(
                            tab: surfaceTab,
                            pasteReadyFiles: { store.pasteReadyFiles(from: surfaceTab) },
                            dismissFileTransfer: { store.dismissFileTransfer(from: surfaceTab) },
                            reconnect: { store.reconnect(surfaceTab) },
                            close: { store.close(surfaceTab) }
                        )
                        .id(surfaceTab.surfaceView.id)
                    }
                    .frame(minWidth: HMuxLayout.terminalVisibleMinWidth, maxWidth: .infinity, maxHeight: .infinity)
                    if store.isInspectorPresented && !store.isConversationPresented && !usesOverlayInspector {
                        HMuxInspectorView(tab: tab) { store.isInspectorPresented = false }
                            .frame(
                                minWidth: HMuxLayout.inspectorMinWidth,
                                idealWidth: HMuxLayout.inspectorIdealWidth,
                                maxWidth: HMuxLayout.inspectorMaxWidth,
                                maxHeight: .infinity
                            )
                    }
                }

                if store.isConversationPresented {
                    HMuxConversationView(session: tab.session)
                        .id(tab.id)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Color.hmuxSurface)
                }

                if store.isInspectorPresented && !store.isConversationPresented && usesOverlayInspector {
                    Color.black.opacity(0.12)
                        .contentShape(Rectangle())
                        .onTapGesture { store.isInspectorPresented = false; store.focusSelectedTerminal() }
                    HMuxInspectorView(tab: tab) { store.isInspectorPresented = false }
                        .frame(
                            width: usesBottomInspector ? geometry.size.width : overlayInspectorWidth,
                            height: usesBottomInspector ? min(280, geometry.size.height * 0.45) : nil
                        )
                        .frame(maxHeight: usesBottomInspector ? nil : .infinity)
                        .overlay(alignment: usesBottomInspector ? .top : .leading) {
                            if usesBottomInspector {
                                Rectangle().fill(Color.hmuxBorder).frame(height: 1)
                            } else {
                                Rectangle().fill(Color.hmuxBorder).frame(width: 1)
                            }
                        }
                        .shadow(color: .black.opacity(0.22), radius: 22, x: usesBottomInspector ? 0 : -8, y: usesBottomInspector ? -8 : 0)
                        .transition(reduceMotion ? .opacity : .move(edge: usesBottomInspector ? .bottom : .trailing).combined(with: .opacity))
                }
            }
        }
    }

}

struct HMuxToolbarButtonStyle: ButtonStyle {
    var isSelected = false
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .labelStyle(.iconOnly)
            .font(.system(size: 13, weight: .medium))
            .foregroundStyle(isSelected ? Color.hmuxSelection : Color.hmuxSecondaryText)
            .frame(width: 28, height: 28)
            .opacity(isEnabled ? 1 : 0.35)
            .background(
                isSelected || configuration.isPressed ? Color.hmuxRaised : Color.clear,
                in: RoundedRectangle(cornerRadius: 7)
            )
    }
}

private struct HMuxEmptyWorkspace: View {
    @ObservedObject var store: HMuxStore

    private var title: String {
        if store.isInitialLoading { return "Connecting to Home" }
        if store.isInitialFailure || store.isCatalogOffline { return "Home is unavailable" }
        if store.isCatalogReconnecting { return "Reconnecting to Home" }
        if store.visibleSessions.isEmpty {
            return store.hiddenRows.isEmpty ? "Start your first session" : "Your sessions are hidden"
        }
        return "Pick up where you left off"
    }

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: store.isInitialFailure || store.isCatalogOffline ? "network.slash" : "terminal")
                .font(.system(size: 32, weight: .light))
                .foregroundStyle(Color.hmuxSelection)
                .frame(width: 76, height: 76)
                .background(Color.hmuxSelection.opacity(0.08), in: RoundedRectangle(cornerRadius: 20))
            Text(title)
                .font(.system(size: 21, weight: .semibold))
                .foregroundStyle(Color.hmuxPrimaryText)
            if store.isInitialLoading || store.isCatalogReconnecting {
                ProgressView().controlSize(.small)
                Text("Waiting for Home. Your work stays on Home while HMux reconnects.")
                    .font(HMuxTypography.body).foregroundStyle(Color.hmuxSecondaryText)
            } else if store.isInitialFailure || store.isCatalogOffline {
                Text("Check your Home connection, then try again. Your remote work stays on Home.")
                    .font(HMuxTypography.body).foregroundStyle(Color.hmuxSecondaryText)
                Button(store.isRefreshing ? "Trying…" : "Try Again") { store.refresh() }
                    .buttonStyle(.borderedProminent).disabled(store.isRefreshing)
                if let message = store.catalogErrorMessage {
                    ScrollView {
                        Text(message).font(HMuxTypography.caption).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(maxWidth: 400, maxHeight: 90)
                    .foregroundStyle(Color.hmuxSecondaryText)
                }
            } else {
                Text("Open an existing session or start something new.\nClosing a tab leaves the session running on Home.")
                    .font(HMuxTypography.body).foregroundStyle(Color.hmuxSecondaryText).lineSpacing(4)
                HStack(spacing: 10) {
                    if !store.visibleSessions.isEmpty {
                        Button("Open Session") { store.presentQuickSwitcher() }
                            .buttonStyle(.borderedProminent)
                    }
                    if store.visibleSessions.isEmpty && !store.hiddenRows.isEmpty {
                        Button("Show Hidden Sessions") { store.presentHiddenSessions() }
                            .buttonStyle(.borderedProminent)
                    }
                    Button("New Session") { store.beginSessionCreation() }
                        .buttonStyle(.bordered).disabled(!store.canCreateSession)
                        .help(store.createSessionUnavailableReason ?? "New Session (⇧⌘N)")
                }
                if store.canReopenClosedTab {
                    Button("Reopen Closed Tab  ⇧⌘T") { store.reopenClosedTab() }
                        .buttonStyle(.link)
                }
                HStack(spacing: 18) {
                    Text("⌘K  Open session")
                    Text("⇧⌘N  New session")
                }
                .font(HMuxTypography.caption).foregroundStyle(Color.hmuxTertiaryText)
            }
        }
        .multilineTextAlignment(.center)
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.hmuxWindow)
    }
}

struct HMuxTabStrip: View {
    @ObservedObject var store: HMuxStore
    var body: some View {
        ScrollViewReader { proxy in
            HStack(spacing: 5) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 4) {
                        ForEach(Array(store.tabs.enumerated()), id: \.element.id) { index, tab in
                            HMuxTabItem(
                                tab: tab,
                                index: index,
                                isSelected: store.selectedTabID == tab.id,
                                select: { store.select(tab) },
                                close: { store.close(tab) }
                            )
                            .id(tab.id)
							.contextMenu {
								Button("Reconnect") { store.reconnect(tab) }.disabled(tab.isMissing)
                                Button("Edit Display Name…") { store.beginAliasEdit(tab.session) }.disabled(tab.isMissing)
                                Button(tab.session.isHidden ? "Show in Sessions" : "Hide from Sessions") {
                                    store.setHidden(!tab.session.isHidden, for: tab.session)
                                }.disabled(tab.isMissing || store.isMutationPending(for: tab.session))
								Divider()
								Button("Move Left") { store.moveTab(tab, by: -1) }.disabled(index == 0)
								Button("Move Right") { store.moveTab(tab, by: 1) }.disabled(index == store.tabs.count - 1)
								Divider()
								Button("Close Tab") { store.close(tab) }
                                Divider()
                                Button("Terminate Session…", role: .destructive) { store.beginTermination(tab.session) }
                                    .disabled(tab.isMissing || store.isMutationPending(for: tab.session))
							}
                        }
                    }
					.padding(.leading, 2)
					.padding(.vertical, 3)
                }

            }
            .onAppear {
                if let selectedID = store.selectedTabID { proxy.scrollTo(selectedID, anchor: .center) }
            }
            .onChange(of: store.tabs.map(\.id)) { _ in
                if let selectedID = store.selectedTabID { proxy.scrollTo(selectedID, anchor: .center) }
            }
            .onChange(of: store.selectedTabID) { selectedID in
                guard let selectedID else { return }
                // Reveal an offscreen tab with the minimum scroll distance;
                // selecting an already visible tab must not recenter the strip.
                proxy.scrollTo(selectedID)
            }
        }
        .frame(height: HMuxLayout.tabBarHeight)
        .overlay {
            Group {
                Button("Previous visual tab") { store.selectPreviousTab() }
                    .keyboardShortcut("[", modifiers: [.command, .shift])
                Button("Next visual tab") { store.selectNextTab() }
                    .keyboardShortcut("]", modifiers: [.command, .shift])
            }
            .disabled(store.hasPresentedModal)
            .frame(width: 0, height: 0)
            .opacity(0)
            .accessibilityHidden(true)
        }
    }
}

struct HMuxTabOverflowMenu: View {
    @ObservedObject var store: HMuxStore

    var body: some View {
        Menu {
            ForEach(Array(store.tabs.enumerated()), id: \.element.id) { index, tab in
                Button {
                    store.select(tab)
                } label: {
                    Label {
						Text(index < 9 ? "⌘\(index + 1)  \(tab.session.displayName)" : tab.session.displayName)
                    } icon: {
                        Image(systemName: tab.session.hmuxStateSymbol)
                    }
                }
            }
			Divider()
			Button("Reopen Closed Tab (⇧⌘T)") { store.reopenClosedTab() }
				.disabled(!store.canReopenClosedTab)
        } label: {
            ZStack(alignment: .topTrailing) {
                Image(systemName: "chevron.down")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Color.hmuxSecondaryText)
					.frame(width: 26, height: 26)
					.background(Color.clear, in: RoundedRectangle(cornerRadius: 6))
                if store.tabs.contains(where: { $0.session.hmuxNeedsAttention }) {
                    Circle()
                        .fill(Color.hmuxWaiting)
                        .frame(width: 6, height: 6)
                        .offset(x: 1, y: -1)
                }
            }
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("All visual tabs")
        .accessibilityLabel("All visual tabs")
    }
}

private struct HMuxTabItem: View {
    @ObservedObject var tab: HMuxTerminalTab
    let index: Int
    let isSelected: Bool
    let select: () -> Void
    let close: () -> Void
    @State private var isHovered = false
    @FocusState private var isCloseFocused: Bool

    var body: some View {
        HStack(spacing: 1) {
            tabButton
            closeButton
        }
        .frame(width: HMuxLayout.tabMaxWidth, height: 26)
        .foregroundStyle(isSelected ? Color.hmuxPrimaryText : Color.hmuxSecondaryText)
        .background(
			isSelected ? Color.hmuxActiveTab : (isHovered ? Color.hmuxHover : Color.hmuxTab),
			in: RoundedRectangle(cornerRadius: 6)
        )
        .overlay {
			RoundedRectangle(cornerRadius: 6)
				.strokeBorder(
					tab.session.hmuxNeedsAttention
						? tab.session.hmuxStateColor.opacity(0.76)
						: (isSelected ? Color.hmuxSelection : Color.hmuxBorder),
					lineWidth: 1
				)
        }
        .onHover { isHovered = $0 }
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    @ViewBuilder
    private var tabButton: some View {
        if index < 9 {
            selectButton.keyboardShortcut(KeyEquivalent(Character(String(index + 1))), modifiers: .command)
        } else {
            selectButton
        }
    }

    private var selectButton: some View {
        Button(action: select) {
            HStack(spacing: 6) {
                if index < 9 {
                    Text("\(index + 1)")
                        .font(.system(size: 9, weight: .semibold, design: .rounded).monospacedDigit())
						.foregroundStyle(isSelected ? Color.hmuxPrimaryText : Color.hmuxTertiaryText)
						.frame(minWidth: 15, minHeight: 16)
						.background(Color.black.opacity(isSelected ? 0.18 : 0.12), in: RoundedRectangle(cornerRadius: 4))
                        .accessibilityHidden(true)
                }
                Text(tab.session.displayName)
					.font(.system(size: 11, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Group {
                if case .ready = tab.fileTransferState {
                    Image(systemName: "paperclip")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Color.hmuxSelection)
                        .help("Uploaded files are ready to insert in this tab")
                } else if tab.session.hmuxNeedsAttention {
                    Image(systemName: "bell.fill")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(tab.session.hmuxStateColor)
                } else {
                    Color.clear
                }
                }
                .frame(width: 12, height: 13)
            }
			.padding(.leading, 7)
			.padding(.trailing, 4)
			.frame(maxWidth: .infinity)
			.frame(height: 26)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(index < 9 ? "Select \(tab.session.displayName) (⌘\(index + 1))" : "Select \(tab.session.displayName)")
		.accessibilityLabel(
			index < 9
				? "\(tab.session.displayName), tab \(index + 1), Command \(index + 1)"
				: tab.session.displayName
		)
    }

    private var closeButton: some View {
        Button(action: close) {
            Image(systemName: "xmark")
                .font(.system(size: 9, weight: .semibold))
				.frame(width: 18, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(Color.hmuxSecondaryText)
        .focused($isCloseFocused)
        .opacity(isHovered || isSelected || isCloseFocused ? 1 : 0)
        .help("Close tab — the Home session keeps running")
        .accessibilityLabel("Close \(tab.session.displayName)")
    }
}

private struct HMuxTerminalCanvas: View {
    @ObservedObject var surfaceView: Ghostty.SurfaceView

    var body: some View {
        Ghostty.SurfaceWrapper(surfaceView: surfaceView)
            // Keep the backing color aligned with terminal theme and OSC updates
            // while the renderer prepares its next frame.
            .background(surfaceView.backgroundColor ?? surfaceView.derivedConfig.backgroundColor)
    }
}

private struct HMuxTerminalSurface: View {
    @ObservedObject var tab: HMuxTerminalTab
    let pasteReadyFiles: () -> Void
    let dismissFileTransfer: () -> Void
    let reconnect: () -> Void
    let close: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            if tab.isMissing {
                connectionNotice(
                    title: "Session ended",
                    detail: "This session is no longer available. The last screen is kept here for reference.",
                    actionTitle: "Close Tab", action: close
                )
            } else {
                TimelineView(.periodic(from: .now, by: 1)) { _ in
                    if tab.isConnectionClosed || tab.surfaceView.processExited {
                        connectionNotice(
                            title: "Connection closed",
                            detail: "Reconnect to continue this session on Home.",
                            actionTitle: "Reconnect", action: reconnect
                        )
                    }
                }
            }
            HMuxFileTransferBanner(state: tab.fileTransferState, paste: pasteReadyFiles, dismiss: dismissFileTransfer)
            HMuxTerminalCanvas(surfaceView: tab.surfaceView)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .transaction { $0.animation = nil; $0.disablesAnimations = true }
    }

    private func connectionNotice(title: String, detail: String, actionTitle: String, action: @escaping () -> Void) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "bolt.horizontal.circle").foregroundStyle(Color.hmuxWaiting)
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(HMuxTypography.label).foregroundStyle(Color.hmuxPrimaryText)
                Text(detail).font(HMuxTypography.caption).foregroundStyle(Color.hmuxSecondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            Button(actionTitle, action: action).buttonStyle(.bordered).controlSize(.small)
        }
        .padding(.horizontal, 12).padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.hmuxChrome)
        .overlay(alignment: .bottom) { Rectangle().fill(Color.hmuxBorder).frame(height: 1) }
    }
}

private struct HMuxFileTransferBanner: View {
	let state: HMuxFileTransferState
	let paste: () -> Void
	let dismiss: () -> Void

	@ViewBuilder
	var body: some View {
		switch state {
		case .idle:
			EmptyView()
		case .staging(_, let fileCount):
			banner {
				ProgressView().controlSize(.small)
				Text("Uploading \(fileCount) \(fileCount == 1 ? "file" : "files") to this Home session…")
				Spacer(minLength: 8)
				Button("Cancel", action: dismiss)
					.buttonStyle(.borderless)
			}
		case .ready(let transfer):
			banner {
				Image(systemName: "checkmark.circle.fill")
					.foregroundStyle(Color.hmuxConnected)
				VStack(alignment: .leading, spacing: 1) {
					Text("Ready to paste \(transfer.result.files.count) \(transfer.result.files.count == 1 ? "file" : "files")")
					Text("Insert the file paths into this terminal without pressing Return.")
						.font(.caption2)
						.foregroundStyle(Color.hmuxTertiaryText)
				}
				Spacer(minLength: 8)
				Button("Insert Paths", action: paste)
					.buttonStyle(.borderedProminent)
					.controlSize(.small)
				Button("Dismiss", action: dismiss)
					.buttonStyle(.borderless)
			}
		case .failed(_, let message):
			banner {
				Image(systemName: "exclamationmark.triangle.fill")
					.foregroundStyle(Color.hmuxFailure)
				Text(message).lineLimit(2)
				Spacer(minLength: 8)
				Button("Dismiss", action: dismiss)
					.buttonStyle(.borderless)
			}
		}
	}

	private func banner<Content: View>(@ViewBuilder content: () -> Content) -> some View {
		HStack(spacing: 8, content: content)
			.font(.caption.weight(.medium))
			.foregroundStyle(Color.hmuxPrimaryText)
			.padding(.horizontal, 11)
			.padding(.vertical, 8)
			.frame(maxWidth: .infinity)
			.background(Color.hmuxChrome)
            .overlay(alignment: .bottom) { Rectangle().fill(Color.hmuxBorder).frame(height: 1) }
			.accessibilityElement(children: .contain)
			.accessibilityLabel(accessibilityLabel)
	}

	private var accessibilityLabel: String {
		switch state {
		case .idle:
			return "File transfer"
		case .staging(_, let fileCount):
			return "Uploading \(fileCount) \(fileCount == 1 ? "file" : "files") to this Home session"
		case .ready(let transfer):
			let fileCount = transfer.result.files.count
			return "\(fileCount) \(fileCount == 1 ? "file is" : "files are") ready to paste"
		case .failed(_, let message):
			return "File transfer failed. \(message)"
		}
	}
}

struct HMuxAttentionLedger: View {
    let summary: HMuxAttentionSummary
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 7) {
                Image(systemName: "exclamationmark.circle.fill")
                    .foregroundStyle(Color.hmuxWaiting)
                if summary.approvals > 0 {
                    HMuxAttentionMetric(symbol: "checkmark.shield", value: summary.approvals)
                }
                if summary.inputs > 0 {
                    HMuxAttentionMetric(symbol: "questionmark.bubble", value: summary.inputs)
                }
                if summary.failures > 0 {
                    HMuxAttentionMetric(symbol: "xmark.circle", value: summary.failures)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Color.hmuxWaiting.opacity(0.12), in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help("Needs attention — approval \(summary.approvals), input \(summary.inputs), failed \(summary.failures). Click to cycle.")
        .accessibilityLabel("\(summary.total) sessions need attention")
    }
}

private struct HMuxAttentionMetric: View {
    let symbol: String
    let value: Int

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: symbol)
            Text("\(value)").monospacedDigit()
        }
        .foregroundStyle(Color.hmuxPrimaryText)
    }
}

import SwiftUI

struct HMuxRootView: View {
    @ObservedObject var store: HMuxStore
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 0) {
            notices
            splitView.transaction { $0.animation = nil; $0.disablesAnimations = true }
        }
			.frame(minWidth: HMuxLayout.windowMinWidth, minHeight: HMuxLayout.windowMinHeight)
			.background(Color.hmuxWindow)
            .hmuxTheme()
			.allowsHitTesting(!store.isQuickSwitcherPresented)
			.disabled(store.isQuickSwitcherPresented)
			.accessibilityHidden(store.isQuickSwitcherPresented)
			.overlay {
				if store.isQuickSwitcherPresented {
					HMuxQuickSwitcherView(store: store)
						.transition(reduceMotion ? .opacity : .scale(scale: 0.98).combined(with: .opacity))
				}
			}
			.sheet(item: $store.aliasEditorSession, onDismiss: store.focusSelectedTerminal) { session in
				HMuxAliasEditorView(store: store, session: session)
                    .background(Color.hmuxChrome)
                    .hmuxTheme()
			}
			.sheet(item: $store.terminationSession, onDismiss: store.focusSelectedTerminal) { session in
				HMuxTerminateSessionView(store: store, session: session)
                    .background(Color.hmuxChrome)
                    .hmuxTheme()
			}
			.sheet(isPresented: $store.isHiddenManagerPresented, onDismiss: store.focusSelectedTerminal) {
				HMuxHiddenSessionsView(store: store)
                    .background(Color.hmuxChrome)
                    .hmuxTheme()
			}
			.sheet(isPresented: $store.isCreateSessionPresented, onDismiss: store.endSessionCreation) {
				HMuxCreateSessionView(store: store)
                    .background(Color.hmuxChrome)
                    .hmuxTheme()
			}
			.overlay {
				Group {
					Button("Open quick switcher") { store.presentQuickSwitcher() }
						.keyboardShortcut("k", modifiers: .command)
						.disabled(store.isQuickSwitcherPresented)
					Button("Focus session search") { store.focusSidebarSearch() }
						.keyboardShortcut("f", modifiers: [.command, .shift])
						.disabled(store.hasPresentedModal)
					Button("Toggle session list") {
						store.isSidebarPresented.toggle()
					}
						.keyboardShortcut("l", modifiers: .command)
					Button("New session") { store.beginSessionCreation() }
						.keyboardShortcut("n", modifiers: [.command, .shift])
						.disabled(!store.canCreateSession || store.hasPresentedModal)
					Button("Reopen closed tab") { store.reopenClosedTab() }
						.keyboardShortcut("t", modifiers: [.command, .shift])
						.disabled(!store.canReopenClosedTab || store.hasPresentedModal)
				}
				.frame(width: 0, height: 0)
				.opacity(0)
				.accessibilityHidden(true)
			}
			.animation(reduceMotion ? nil : .easeOut(duration: 0.16), value: store.actionErrorMessage)
			.animation(reduceMotion ? nil : .easeOut(duration: 0.16), value: store.appUpdate)
			.animation(reduceMotion ? nil : .easeOut(duration: 0.16), value: store.isQuickSwitcherPresented)
			.onChange(of: store.isSidebarPresented) { visible in
                store.persistWorkspace()
                if !visible { store.focusSelectedTerminal() }
            }
			.onChange(of: store.isInspectorPresented) { visible in
                store.persistWorkspace()
                if !visible { store.focusSelectedTerminal() }
            }
	}

    private var notices: some View {
				VStack(spacing: 0) {
					if let update = store.appUpdate {
						HMuxUpdateBanner(version: update.version, restart: store.restartForUpdate)
							.transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
					}
					if let message = store.actionErrorMessage {
						HMuxErrorBanner(message: message, dismiss: store.dismissActionError)
							.transition(reduceMotion ? .opacity : .move(edge: .top).combined(with: .opacity))
					}
				}
    }

	@ViewBuilder
	private var splitView: some View {
        GeometryReader { geometry in
            navigationSplitView
                .toolbar { toolbarActions(tabWidth: max(188, geometry.size.width - 360)) }
                .toolbarBackground(Color.hmuxChrome, for: .windowToolbar)
                .toolbarBackground(.visible, for: .windowToolbar)
        }
	}

	@ToolbarContentBuilder
	private func toolbarActions(tabWidth: CGFloat) -> some ToolbarContent {
        ToolbarItem(placement: .principal) {
            HMuxTabStrip(store: store)
                .frame(width: tabWidth)
        }
        ToolbarItem(placement: .primaryAction) {
            HStack(spacing: 2) {
                Button {
                    store.setConversationPresented(!store.isConversationPresented)
                } label: {
                    Image(systemName: store.isConversationPresented ? "terminal" : "text.alignleft")
                }
                .buttonStyle(HMuxToolbarButtonStyle(isSelected: store.isConversationPresented))
                .help(store.isConversationPresented ? "Return to terminal (⇧⌘D)" : "Read this tab’s Codex conversation (⇧⌘D)")
                .accessibilityLabel(store.isConversationPresented ? "Return to terminal" : "Read this tab’s Codex conversation")
                .keyboardShortcut("d", modifiers: [.command, .shift])
                .disabled(store.hasPresentedModal || store.selectedTab == nil || (!store.isConversationPresented && store.selectedTab?.isMissing == true))
                HMuxTabOverflowMenu(store: store)
                    .disabled(store.tabs.isEmpty || store.hasPresentedModal)
            }
        }
        ToolbarItem(placement: .primaryAction) {
            Button { store.presentQuickSwitcher() } label: {
                Label("Open Session", systemImage: "magnifyingglass")
            }
            .buttonStyle(HMuxToolbarButtonStyle())
            .help("Open an existing session (⌘K)")
            .disabled(store.hasPresentedModal)
        }
        ToolbarItem(placement: .primaryAction) {
            Button { store.beginSessionCreation() } label: {
                Label("New Session", systemImage: "plus")
            }
            .buttonStyle(HMuxToolbarButtonStyle())
            .disabled(!store.canCreateSession || store.hasPresentedModal)
            .help(store.createSessionUnavailableReason ?? "New Session (⇧⌘N)")
        }
		ToolbarItem(placement: .primaryAction) {
			Button {
				store.isInspectorPresented.toggle()
			} label: {
				Image(systemName: "sidebar.right")
			}
			.buttonStyle(HMuxToolbarButtonStyle(isSelected: store.isInspectorPresented))
			.keyboardShortcut("i", modifiers: [.command, .option])
			.disabled(store.selectedTab == nil || store.hasPresentedModal || store.isConversationPresented)
			.help("Toggle session details (⌥⌘I)")
			.accessibilityLabel("Toggle session details")
		}
	}

	private var navigationSplitView: some View {
        NavigationSplitView(columnVisibility: Binding(
			get: { store.isSidebarPresented ? .all : .detailOnly },
			set: { store.isSidebarPresented = $0 != .detailOnly }
		)) {
            HMuxSidebarView(store: store)
                .navigationSplitViewColumnWidth(
                    min: HMuxLayout.sidebarMinWidth,
                    ideal: HMuxLayout.sidebarIdealWidth,
                    max: HMuxLayout.sidebarMaxWidth
                )
        } detail: {
            HMuxWorkspaceView(store: store)
        }
        .navigationSplitViewStyle(.balanced)
    }
}

private struct HMuxUpdateBanner: View {
    let version: String
    let restart: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "checkmark.seal.fill")
                .foregroundStyle(Color.hmuxConnected)
            VStack(alignment: .leading, spacing: 2) {
                Text("HMux \(version) is ready")
                    .font(.caption.weight(.semibold))
                Text("Restart to use the updated app.")
                    .font(HMuxTypography.micro)
                    .foregroundStyle(Color.hmuxSecondaryText)
            }
            Spacer(minLength: 12)
            Button("Restart", action: restart)
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.hmuxRaised)
        .overlay(alignment: .bottom) { Rectangle().fill(Color.hmuxBorder).frame(height: 1) }
        .accessibilityElement(children: .contain)
    }
}

private struct HMuxErrorBanner: View {
    let message: String
    let dismiss: () -> Void
    @State private var showsDetails = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(Color.hmuxFailure)
            Text(message)
                .font(.caption)
                .foregroundStyle(Color.hmuxPrimaryText)
                .lineLimit(2)
            Spacer(minLength: 8)
            Button("Details") { showsDetails.toggle() }
                .buttonStyle(.borderless)
                .popover(isPresented: $showsDetails) {
                    ScrollView {
                        Text(message).font(HMuxTypography.body).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(16)
                    }
                    .frame(width: 400, height: 220)
                    .background(Color.hmuxChrome)
                    .hmuxTheme()
                }
            Button(action: dismiss) {
                Image(systemName: "xmark")
                    .font(.caption.weight(.semibold))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Dismiss error")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.hmuxRaised)
        .overlay(alignment: .bottom) { Rectangle().fill(Color.hmuxBorder).frame(height: 1) }
        .accessibilityElement(children: .contain)
    }
}

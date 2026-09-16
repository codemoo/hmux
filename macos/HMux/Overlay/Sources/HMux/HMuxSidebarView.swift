import SwiftUI

struct HMuxSidebarView: View {
    @ObservedObject var store: HMuxStore
    @State private var searchFocused = false

    var body: some View {
        VStack(spacing: 0) {
            catalogHeader
            Rectangle().fill(Color.hmuxBorder).frame(height: 1)
            sessionList
            Rectangle().fill(Color.hmuxBorder).frame(height: 1)
            catalogFooter
        }
		.background(Color.hmuxSidebar)
		.onAppear {
			if store.sidebarSearchFocusRequest > 0 {
				searchFocused = true
				store.consumeSidebarSearchFocusRequest()
			}
		}
		.onChange(of: store.sidebarSearchFocusRequest) { request in
			guard request > 0 else { return }
			searchFocused = true
			store.consumeSidebarSearchFocusRequest()
		}
		.onChange(of: searchFocused) { focused in
			store.setSidebarSearchInteraction(focused)
		}
		.onDisappear { store.setSidebarSearchInteraction(false) }
    }

    private var catalogHeader: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                HStack(spacing: 8) {
                    ZStack {
                        RoundedRectangle(cornerRadius: 7).fill(Color.hmuxSelection)
                        Image(systemName: "rectangle.3.group.fill")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Color.hmuxOnAccent)
                    }
                    .frame(width: 24, height: 24)
                    .accessibilityHidden(true)
                    Text("Sessions")
                }
                    .font(HMuxTypography.title)
                    .foregroundStyle(Color.hmuxPrimaryText)
                Text("\(store.visibleSessions.count)")
                    .font(HMuxTypography.caption.monospacedDigit())
                    .foregroundStyle(Color.hmuxSecondaryText)
                Spacer()
                if !store.attentionSummary.isEmpty {
                    HMuxAttentionCountBadge(count: store.attentionSummary.total)
                }
				Button {
					store.beginSessionCreation()
				} label: {
					Image(systemName: "plus")
						.font(.system(size: 12, weight: .semibold))
						.frame(width: 24, height: 24)
						.contentShape(Rectangle())
				}
				.buttonStyle(.plain)
				.disabled(!store.canCreateSession)
				.help(store.createSessionUnavailableReason ?? "New Session (⇧⌘N)")
				.accessibilityLabel("New Session")
                Menu {
                    Button {
                        store.presentHiddenSessions()
                    } label: {
                        Label("Hidden Sessions (\(store.hiddenRows.count))", systemImage: "eye.slash")
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 12, weight: .semibold))
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle())
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help("Session management")
                .accessibilityLabel("Session management")
            }

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Color.hmuxSecondaryText)
                HMuxSearchField(
                    placeholder: "Search sessions", text: $store.searchText, isFocused: $searchFocused,
                    onSubmit: openFirstFilteredSession, onCancel: clearSearchAndReturnToTerminal
                )
                .frame(minWidth: 0, maxWidth: .infinity)
                .frame(height: 18)
                    Button {
                        store.searchText = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Color.hmuxSecondaryText)
                    .accessibilityLabel("Clear search")
                    .frame(width: 14, height: 18)
                    .opacity(store.searchText.isEmpty ? 0 : 1)
                    .disabled(store.searchText.isEmpty)
                    .accessibilityHidden(store.searchText.isEmpty)
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .background(Color.hmuxRaised, in: RoundedRectangle(cornerRadius: 8))
            .overlay {
                RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(searchFocused ? Color.hmuxSelection : Color.hmuxBorder, lineWidth: 1)
            }

            HStack(spacing: 4) {
                ForEach(HMuxSessionFilter.allCases) { filter in
                    Button {
                        store.sessionFilter = filter
                    } label: {
                        Text(filter.rawValue)
                            .font(HMuxTypography.label)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 5)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(store.sessionFilter == filter ? Color.hmuxPrimaryText : Color.hmuxSecondaryText)
                    .background(
                        store.sessionFilter == filter ? Color.hmuxSurface : Color.clear,
                        in: RoundedRectangle(cornerRadius: 7)
                    )
                    .accessibilityAddTraits(store.sessionFilter == filter ? .isSelected : [])
                }
            }
            .padding(3)
            .background(Color.hmuxRaised, in: RoundedRectangle(cornerRadius: 9))
        }
        .padding(.horizontal, HMuxSpacing.medium)
        .padding(.top, HMuxSpacing.medium)
        .padding(.bottom, HMuxSpacing.medium)
    }

    @ViewBuilder
    private var sessionList: some View {
        if store.isInitialLoading {
            HMuxCatalogSkeleton()
        } else if store.isInitialFailure {
            HMuxCatalogFailure(message: store.catalogErrorMessage ?? "Catalog unavailable") {
                store.refresh()
            }
        } else if store.filteredSessionRows.isEmpty {
            HMuxCatalogEmptyState(
				state: emptyState,
				canCreateSession: store.canCreateSession,
				creationUnavailableReason: store.createSessionUnavailableReason,
				clearSearch: { store.searchText = "" },
				showAll: showAllSessions,
				showHidden: { store.presentHiddenSessions() },
				newSession: { store.beginSessionCreation() }
            )
        } else {
            ScrollView(.vertical) {
				HMuxSessionList(rows: store.filteredSessionRows, store: store)
                .padding(.horizontal, 9)
                .padding(.vertical, 10)
            }
            .scrollIndicators(.automatic)
        }
    }

    private var catalogFooter: some View {
		Group {
			if catalogFailure {
				Button {
					isCatalogDetailsPresented.toggle()
				} label: {
					footerStatus
				}
				.buttonStyle(.plain)
				.popover(isPresented: $isCatalogDetailsPresented, arrowEdge: .top) {
					catalogFailureDetails
				}
			} else {
				footerStatus
			}
		}
		.padding(.horizontal, 11)
		.frame(height: 30)
		.accessibilityElement(children: .contain)
    }

	@State private var isCatalogDetailsPresented = false

	private var catalogFailure: Bool { store.isCatalogOffline || store.isInitialFailure }

	private var catalogStatusLabel: String {
		if catalogFailure { return "Offline" }
		if store.isCatalogReconnecting { return "Reconnecting…" }
		return store.isCatalogConnected ? "Connected" : "Connecting…"
	}

	private var catalogStatusColor: Color {
		if catalogFailure { return Color.hmuxFailure }
		if store.isCatalogReconnecting { return Color.hmuxWaiting }
		return store.isCatalogConnected ? Color.hmuxConnected : Color.hmuxTertiaryText
	}

	private var footerStatus: some View {
		HStack(spacing: 7) {
			Circle().fill(catalogStatusColor).frame(width: 6, height: 6)
			Text(catalogStatusLabel)
                .help(store.sharedWorkspaceStatus)
			Spacer()
			if catalogFailure {
				Image(systemName: "chevron.up.chevron.down")
					.font(.system(size: 9, weight: .semibold))
			}
		}
		.font(HMuxTypography.micro)
		.foregroundStyle(catalogStatusColor)
		.contentShape(Rectangle())
	}

	private var catalogFailureDetails: some View {
		VStack(alignment: .leading, spacing: 10) {
			Text("Catalog unavailable")
				.font(HMuxTypography.rowTitle)
			ScrollView(.vertical) {
				Text(store.catalogErrorMessage ?? "The catalog connection is unavailable.")
					.font(HMuxTypography.caption)
					.foregroundStyle(Color.hmuxSecondaryText)
					.textSelection(.enabled)
					.fixedSize(horizontal: false, vertical: true)
			}
			.frame(maxHeight: 180)
			Button("Retry") {
				isCatalogDetailsPresented = false
				store.refresh()
			}
			.buttonStyle(.borderedProminent)
			.controlSize(.small)
		}
		.padding(HMuxSpacing.medium)
		.frame(width: 320, alignment: .leading)
        .background(Color.hmuxChrome)
        .hmuxTheme()
	}

	private var emptyState: HMuxCatalogEmptyState.State {
		if store.sessionFilter != .all { return .filter }
		if !store.searchText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return .query }
		if !store.hiddenRows.isEmpty { return .allHidden }
		return .noVisibleSessions
	}

	private func showAllSessions() {
		store.searchText = ""
		store.sessionFilter = .all
	}

	private func openFirstFilteredSession() {
		guard let first = store.filteredSessionRows.first else { return }
        searchFocused = false
        store.setSidebarSearchInteraction(false)
		_ = store.open(first.session)
	}

	private func clearSearchAndReturnToTerminal() {
		store.searchText = ""
		searchFocused = false
        store.setSidebarSearchInteraction(false)
		store.focusSelectedTerminal()
	}
}

private struct HMuxAttentionCountBadge: View {
    let count: Int

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "exclamationmark")
            Text("\(count)").monospacedDigit()
        }
        .font(HMuxTypography.micro)
        .foregroundStyle(Color.hmuxWaiting)
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(Color.hmuxWaiting.opacity(0.12), in: Capsule())
        .accessibilityLabel("\(count) sessions need attention")
    }
}

private struct HMuxBlockedDurationBadge: View {
    let text: String
    let color: Color

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "clock.fill")
            Text(text).monospacedDigit()
        }
        .font(HMuxTypography.micro)
        .foregroundStyle(color)
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
        .background(color.opacity(0.12), in: Capsule())
    }
}

private struct HMuxCatalogSkeleton: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("CONNECTING")
                .font(HMuxTypography.micro)
                .tracking(0.6)
                .foregroundStyle(Color.hmuxTertiaryText)
            ForEach(0..<4, id: \.self) { index in
                HStack(spacing: 10) {
                    Circle().fill(Color.hmuxRaised).frame(width: 22, height: 22)
                    VStack(alignment: .leading, spacing: 7) {
                        RoundedRectangle(cornerRadius: 3)
                            .fill(Color.hmuxRaised)
                            .frame(width: index.isMultiple(of: 2) ? 132 : 164, height: 11)
                        RoundedRectangle(cornerRadius: 3)
                            .fill(Color.hmuxRaised.opacity(0.75))
                            .frame(width: index.isMultiple(of: 2) ? 184 : 144, height: 9)
                    }
                }
                .padding(.horizontal, 8)
                .frame(height: 48)
                .accessibilityHidden(true)
            }
            Spacer()
        }
        .padding(HMuxSpacing.medium)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityLabel("Connecting to session catalog")
    }
}

private struct HMuxCatalogFailure: View {
    let message: String
    let retry: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "bolt.horizontal.circle")
                .font(.system(size: 28, weight: .light))
                .foregroundStyle(Color.hmuxFailure)
            Text("Catalog unavailable")
                .font(HMuxTypography.rowTitle)
                .foregroundStyle(Color.hmuxPrimaryText)
            Text(message)
                .font(HMuxTypography.caption)
                .foregroundStyle(Color.hmuxSecondaryText)
                .lineLimit(3)
                .multilineTextAlignment(.center)
            Button("Try Again", action: retry)
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
        }
        .padding(HMuxSpacing.xLarge)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}

private struct HMuxCatalogEmptyState: View {
    enum State {
		case query
		case filter
		case allHidden
		case noVisibleSessions
	}

	let state: State
	let canCreateSession: Bool
	let creationUnavailableReason: String?
	let clearSearch: () -> Void
	let showAll: () -> Void
    let showHidden: () -> Void
	let newSession: () -> Void

    var body: some View {
        VStack(spacing: 9) {
			Image(systemName: icon)
                .font(.system(size: 24, weight: .light))
			Text(title)
                .font(HMuxTypography.rowTitle)
			Text(message)
                .font(HMuxTypography.caption)
                .multilineTextAlignment(.center)
			action
        }
        .foregroundStyle(Color.hmuxSecondaryText)
        .padding(HMuxSpacing.xLarge)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

	@ViewBuilder
	private var action: some View {
		switch state {
		case .query:
			Button("Clear Search", action: clearSearch).buttonStyle(.link)
		case .filter:
			Button("Show All Sessions", action: showAll).buttonStyle(.borderedProminent).controlSize(.small)
		case .allHidden:
			Button("Show Hidden Sessions", action: showHidden).buttonStyle(.link)
		case .noVisibleSessions:
			Button("New Session", action: newSession)
				.buttonStyle(.borderedProminent)
				.controlSize(.small)
				.disabled(!canCreateSession)
				.help(creationUnavailableReason ?? "New Session")
		}
	}

	private var icon: String {
		switch state {
		case .query, .filter: return "magnifyingglass"
		case .allHidden: return "eye.slash"
		case .noVisibleSessions: return "rectangle.stack"
		}
	}

	private var title: String {
		switch state {
		case .query: return "No matching sessions"
		case .filter: return "No sessions in this filter"
		case .allHidden: return "All sessions are hidden"
		case .noVisibleSessions: return "No visible sessions"
		}
	}

	private var message: String {
		switch state {
		case .query: return "Try a session, project, model, or tag."
		case .filter: return "Show all sessions to clear the current search and filter."
		case .allHidden: return "Restore a hidden session to return it to the catalog."
		case .noVisibleSessions: return "Create a session when the Home connection is ready."
		}
	}
}

private struct HMuxSessionList: View {
    let rows: [HMuxSessionRowState]
    @ObservedObject var store: HMuxStore

    var body: some View {
		let openTabIDs = store.openTabIDs
		LazyVStack(alignment: .leading, spacing: 5) {
			ForEach(rows) { row in
				HMuxSessionRow(
					row: row,
					isSelected: store.selectedTabID == row.id,
					isOpen: openTabIDs.contains(row.id),
					isPending: store.isMutationPending(for: row.session),
					editAlias: { store.beginAliasEdit(row.session) },
					hide: { store.setHidden(true, for: row.session) },
					terminate: { store.beginTermination(row.session) }
				) {
					store.open(row.session)
				}
			}
		}
    }
}

private struct HMuxSessionRow: View {
    @ObservedObject var row: HMuxSessionRowState
    let isSelected: Bool
    let isOpen: Bool
    let isPending: Bool
    let editAlias: () -> Void
    let hide: () -> Void
    let terminate: () -> Void
    let action: () -> Void
    @State private var isHovered = false

    private var session: HMuxSession { row.session }

    var body: some View {
        HStack(spacing: 2) {
            Button(action: action) {
                rowContent
            }
            .buttonStyle(.plain)

            Menu {
                Button(action: editAlias) {
                    Label("Edit Display Name…", systemImage: "pencil")
                }
                Button(action: hide) {
                    Label("Hide Session", systemImage: "eye.slash")
                }
                Divider()
                Button(role: .destructive, action: terminate) {
                    Label("Terminate Session…", systemImage: "trash")
                }
            } label: {
                Group {
                    if isPending {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "ellipsis")
                            .font(.system(size: 11, weight: .semibold))
                    }
                }
                .foregroundStyle(Color.hmuxSecondaryText)
                .frame(width: 25, height: 30)
                .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .disabled(isPending)
            .help("Manage \(session.displayName)")
            .accessibilityLabel("Manage \(session.displayName)")
        }
        .background(rowBackground, in: RoundedRectangle(cornerRadius: HMuxLayout.rowRadius))
        .overlay(alignment: .leading) {
            if session.hmuxNeedsAttention {
                Capsule()
                    .fill(session.hmuxStateColor)
                    .frame(width: 3)
                    .padding(.vertical, 7)
                    .accessibilityHidden(true)
            }
        }
        .onHover { isHovered = $0 }
        .contextMenu {
            Button("Open Session", action: action)
            Button("Edit Display Name…", action: editAlias)
                .disabled(isPending)
            Button("Hide Session", action: hide)
                .disabled(isPending)
            Divider()
            Button("Terminate Session…", role: .destructive, action: terminate)
                .disabled(isPending)
        }
        .help(session.currentPath.isEmpty ? session.displayName : session.currentPath)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var rowContent: some View {
            HStack(alignment: .center, spacing: 10) {
                ZStack {
                    Circle()
                        .fill(session.hmuxStateColor.opacity(0.14))
                        .frame(width: 22, height: 22)
                    Image(systemName: session.hmuxStateSymbol)
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(session.hmuxStateColor)
                }

                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(session.displayName)
                            .font(HMuxTypography.rowTitle)
                            .foregroundStyle(Color.hmuxPrimaryText)
                            .lineLimit(1)
                        Spacer(minLength: 4)
                        if session.hmuxNeedsAttention {
                            TimelineView(.periodic(from: .now, by: 30)) { context in
                                HMuxBlockedDurationBadge(
                                    text: hmuxCompactDuration(since: session.hmuxAttentionDate, until: context.date),
                                    color: session.hmuxStateColor
                                )
                            }
                        } else {
                            Text(session.hmuxActivityDate, style: .relative)
                                .font(HMuxTypography.micro.monospacedDigit())
                                .foregroundStyle(Color.hmuxTertiaryText)
                                .fixedSize()
                        }
                        if isOpen {
                            Circle().fill(Color.hmuxSelection).frame(width: 6, height: 6)
                                .accessibilityLabel("Open")
                        }
                    }

                    HStack(spacing: 5) {
                        Text(session.hmuxRuntimeLabel).fontWeight(.medium)
                        if let model = session.modelLabel {
                            Text("·")
                            Text(model).lineLimit(1)
                        }
                        Text("·")
                        Text(session.hmuxProjectName).lineLimit(1)
                        if session.attachedClients > 0 {
                            Spacer(minLength: 3)
                            HMuxMiniBadge(text: "\(session.attachedClients)", symbol: "link")
                        }
                    }
                    .font(HMuxTypography.label)
                    .foregroundStyle(Color.hmuxSecondaryText)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .contentShape(Rectangle())
    }

    private var rowBackground: Color {
        if isSelected { return Color.hmuxSelectedBackground }
        if session.hmuxNeedsAttention { return session.hmuxStateColor.opacity(0.07) }
        if isHovered { return Color.hmuxRaised.opacity(0.9) }
        return Color.clear
    }

    private var accessibilityLabel: String {
        var parts = [session.displayName, session.hmuxStateLabel, session.hmuxRuntimeLabel]
        if let model = session.modelLabel { parts.append(model) }
        if session.hmuxNeedsAttention {
            parts.append("waiting \(hmuxCompactDuration(since: session.hmuxAttentionDate))")
        }
        if !session.currentPath.isEmpty { parts.append(session.currentPath) }
        return parts.joined(separator: ", ")
    }
}

private struct HMuxMiniBadge: View {
    let text: String
    let symbol: String

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: symbol).font(.system(size: 8, weight: .semibold))
            Text(text).monospacedDigit()
        }
        .font(HMuxTypography.micro)
        .foregroundStyle(Color.hmuxSecondaryText)
        .padding(.horizontal, 5)
        .padding(.vertical, 2)
        .background(Color.hmuxRaised, in: Capsule())
    }
}

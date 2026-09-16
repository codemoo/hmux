import AppKit
import SwiftUI

struct HMuxWorkspaceFooter: View {
    @ObservedObject var store: HMuxStore
    @ObservedObject private var usage: HMuxTokenUsageStore
    @State private var showsUsageDetails = false

    init(store: HMuxStore) {
        self.store = store
        _usage = ObservedObject(wrappedValue: store.usageStore)
    }

    var body: some View {
        GeometryReader { geometry in
            let compact = geometry.size.width < 760
            HStack(spacing: 8) {
                if !compact {
                    HMuxRunningBedlView(state: usage.burnState)
                        .frame(width: 32, height: 22)
                }
                usageButton(compact: compact)
                    .fixedSize(horizontal: true, vertical: false)
                HMuxHostMetricsView(store: store.hostMetricsStore)
                    .fixedSize(horizontal: true, vertical: false)

                Spacer(minLength: 0)
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) {
                        if !store.isSidebarPresented {
                            HMuxCompactConnectionStatus(store: store)
                        }
                        if let tab = store.selectedTab, !tab.session.currentPath.isEmpty {
                            HMuxWorkingDirectoryLabel(session: tab.session)
                        }
                        attentionButton
                    }
                    attentionButton
                    Color.clear.frame(width: 0, height: 0)
                }
            }
            .font(HMuxTypography.micro)
            .foregroundStyle(Color.hmuxTertiaryText)
            .frame(height: 22, alignment: .center)
            .padding(.vertical, 5)
            .padding(.horizontal, 9)
            .frame(width: geometry.size.width, height: HMuxLayout.statusBarHeight, alignment: .center)
        }
        .frame(height: HMuxLayout.statusBarHeight)
        .background(Color.hmuxWindow)
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private var attentionButton: some View {
        if !store.attentionSummary.isEmpty {
            HMuxAttentionLedger(summary: store.attentionSummary) {
                store.focusNextAttentionSession()
            }
        }
    }

    private func usageButton(compact: Bool) -> some View {
        Button {
            showsUsageDetails.toggle()
        } label: {
            HStack(spacing: 7) {
                if !compact {
                    Image(systemName: "gauge.with.dots.needle.67percent")
                        .font(.system(size: 11, weight: .semibold))
                }
                HMuxCompactUsageMetric(summary: usage.summary(for: .claude), compact: compact)
                HMuxCompactUsageMetric(summary: usage.summary(for: .codex), compact: compact)
                if compact {
                    Text("left · 1w").foregroundStyle(Color.hmuxTertiaryText)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 22)
            .contentShape(RoundedRectangle(cornerRadius: 7))
        }
        .buttonStyle(.plain)
        .foregroundStyle(showsUsageDetails ? Color.hmuxSelection : Color.hmuxSecondaryText)
        .background(Color.hmuxRaised.opacity(showsUsageDetails ? 1 : 0.72), in: RoundedRectangle(cornerRadius: 7))
        .overlay { RoundedRectangle(cornerRadius: 7).strokeBorder(Color.hmuxBorder.opacity(0.7), lineWidth: 1) }
        .keyboardShortcut("u", modifiers: [.command, .option])
        .help("Claude and Codex weekly usage details (⌥⌘U)")
        .accessibilityLabel(usageAccessibilityLabel)
        .popover(isPresented: $showsUsageDetails, arrowEdge: .bottom) {
            HMuxUsageDetailsView(usage: usage).hmuxTheme()
        }
    }

	private var usageAccessibilityLabel: String {
		let summaries = HMuxUsageProvider.allCases.map { provider -> String in
			let summary = usage.summary(for: provider)
			if let remaining = summary.remainingPercent {
				return "\(provider.label) \(remaining) percent left this week"
			}
			return "\(provider.label) \(summary.status.accessibilityLabel)"
		}
		return "Usage details, " + summaries.joined(separator: ", ")
	}
}

private extension HMuxUsageSummaryStatus {
	var accessibilityLabel: String {
		switch self {
		case .quota: return "quota unavailable"
		case .starting: return "starting"
		case .offline: return "Home stream offline"
		case .homeSession: return "Home sign-in needed"
		case .network: return "provider unavailable"
		case .limited: return "provider rate limited"
		case .changed: return "usage format changed"
		case .updateRequired: return "Home agent update needed"
		case .unavailable: return "no usage data"
		}
	}
}

private struct HMuxCompactUsageMetric: View {
    let summary: HMuxUsageSummary
    var compact = false

    var body: some View {
        HStack(spacing: 4) {
            Text(summary.isAccountPool ? "Codex LB" : summary.provider.label)
                .foregroundStyle(Color.hmuxSecondaryText)
            Text(metricText)
                .font(.system(size: 10, weight: .semibold, design: .rounded).monospacedDigit())
                .foregroundStyle(metricColor)
            if summary.stale {
                Image(systemName: "clock.badge.exclamationmark")
                    .font(.system(size: 8, weight: .medium))
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    private var metricColor: Color {
		guard summary.status == .quota, let remaining = summary.remainingPercent else {
			switch summary.status {
			case .homeSession, .limited, .changed, .updateRequired: return Color.hmuxWaiting
			case .network, .offline: return Color.hmuxFailure
			default: return Color.hmuxTertiaryText
			}
		}
        if summary.stale { return Color.hmuxTertiaryText }
        if remaining <= 15 { return Color.hmuxFailure }
        if remaining <= 35 { return Color.hmuxWaiting }
        return Color.hmuxConnected
    }

    private var accessibilityLabel: String {
		let value = summary.remainingPercent.map { "\($0) percent remaining this week" } ?? metricText
        return "\(summary.provider.label), \(value)\(summary.stale ? ", stale" : "")"
    }

	private var metricText: String {
		if let remaining = summary.remainingPercent { return compact ? "\(remaining)%" : "\(remaining)% left · 1w" }
        if compact { return "—" }
		switch summary.status {
		case .quota: return "—"
		case .starting: return "Starting"
		case .offline: return "Offline"
		case .homeSession: return "Home sign-in"
		case .network: return "Provider offline"
		case .limited: return "Rate limited"
		case .changed: return "Format changed"
		case .updateRequired: return "Agent update"
		case .unavailable: return "No data"
		}
	}
}

private struct HMuxCompactConnectionStatus: View {
	@ObservedObject var store: HMuxStore

	var body: some View {
		HStack(spacing: 5) {
			Image(systemName: symbol)
				.font(.system(size: 8, weight: .bold))
			Text(store.connectionLabel)
				.lineLimit(1)
		}
		.foregroundStyle(color)
		.help("Home catalog: \(store.connectionLabel)")
		.accessibilityElement(children: .combine)
		.accessibilityLabel("Home catalog \(store.connectionLabel)")
	}

	private var symbol: String {
		if store.isCatalogReconnecting { return "arrow.triangle.2.circlepath" }
		if store.isCatalogOffline { return "exclamationmark.circle.fill" }
		return store.isCatalogConnected ? "checkmark.circle.fill" : "circle.dotted"
	}

	private var color: Color {
		if store.isCatalogOffline { return Color.hmuxFailure }
		if store.isCatalogReconnecting { return Color.hmuxWaiting }
		return store.isCatalogConnected ? Color.hmuxConnected : Color.hmuxTertiaryText
	}
}

private struct HMuxWorkingDirectoryLabel: View {
	let session: HMuxSession

	var body: some View {
		Label {
			Text(session.hmuxProjectName)
				.lineLimit(1)
				.truncationMode(.middle)
		} icon: {
			Image(systemName: "folder")
		}
		.layoutPriority(-1)
		.help(session.currentPath)
		.contextMenu {
			Button("Copy Working Directory") { copyWorkingDirectory() }
		}
		.accessibilityElement(children: .ignore)
		.accessibilityLabel("Working directory \(session.currentPath)")
	}

	private func copyWorkingDirectory() {
		let pasteboard = NSPasteboard.general
		pasteboard.clearContents()
		pasteboard.setString(session.currentPath, forType: .string)
	}
}

private struct HMuxUsageDetailsView: View {
    @ObservedObject var usage: HMuxTokenUsageStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                ZStack {
                    RoundedRectangle(cornerRadius: 9)
                        .fill(Color.hmuxSelection.opacity(0.14))
                    Image(systemName: "gauge.with.dots.needle.67percent")
                        .foregroundStyle(Color.hmuxSelection)
                }
                .frame(width: 34, height: 34)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Token usage")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(Color.hmuxPrimaryText)
                    Text("Weekly quota from your Home accounts")
                        .font(HMuxTypography.micro)
                        .foregroundStyle(Color.hmuxSecondaryText)
                }
                Spacer()
                HMuxRunningBedlView(state: usage.burnState)
                    .frame(width: 42, height: 31)
            }
            .padding(14)

            Divider().overlay(Color.hmuxBorder)

            ScrollView {
                VStack(spacing: 10) {
                    HMuxProviderUsageCard(provider: .claude, state: usage.claude)
                    HMuxProviderUsageCard(provider: .codex, state: usage.codex)
                }
                .padding(12)
            }
            .frame(maxHeight: 620)
        }
        .frame(width: 420)
        .background(Color.hmuxSurface)
    }
}

private struct HMuxProviderUsageCard: View {
    let provider: HMuxUsageProvider
    let state: HMuxProviderUsageState

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
				VStack(alignment: .leading, spacing: 1) {
					Text(provider.label)
						.font(.system(size: 13, weight: .semibold))
						.foregroundStyle(Color.hmuxPrimaryText)
					if let source = sourceLabel {
						Text(source)
							.font(HMuxTypography.micro)
							.foregroundStyle(Color.hmuxTertiaryText)
					}
				}
                Spacer()
                stateChip
            }

			if let snapshot = state.snapshot, state.hasUsableQuota {
                HStack(alignment: .firstTextBaseline) {
                    Text(remainingText(snapshot))
                        .font(.system(size: 24, weight: .semibold, design: .rounded).monospacedDigit())
                        .foregroundStyle(Color.hmuxPrimaryText)
					Text("left · 1w")
                        .font(HMuxTypography.micro)
                        .foregroundStyle(Color.hmuxSecondaryText)
                    Spacer()
                    Text(HMuxUsageFormat.tokens(snapshot.todayTotalTokens))
                        .font(HMuxTypography.micro.monospacedDigit())
                        .foregroundStyle(Color.hmuxSecondaryText)
                    Text("today")
                        .font(HMuxTypography.micro)
                        .foregroundStyle(Color.hmuxTertiaryText)
                }

                ForEach(HMuxUsageFormat.windows(for: snapshot), id: \.label) { window in
                    HMuxUsageWindowRow(window: window)
                }

                if state.isStale {
                    Label("Last value is stale", systemImage: "clock.badge.exclamationmark")
                        .font(HMuxTypography.micro)
                        .foregroundStyle(Color.hmuxWaiting)
                }
				if let retryMessage {
					Label(retryMessage, systemImage: "clock.arrow.circlepath")
						.font(HMuxTypography.micro)
						.foregroundStyle(Color.hmuxWaiting)
				}
            } else {
                VStack(alignment: .leading, spacing: 4) {
					Text(unavailableTitle)
                        .font(HMuxTypography.caption.weight(.semibold))
                        .foregroundStyle(Color.hmuxSecondaryText)
					Text(unavailableMessage)
                        .font(HMuxTypography.micro)
                        .foregroundStyle(Color.hmuxTertiaryText)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            if let snapshot = state.snapshot {
                if let accounts = snapshot.accounts, !accounts.isEmpty {
                    Divider().overlay(Color.hmuxBorder.opacity(0.7))
                    HStack {
                        Text("Accounts · 1w")
                            .font(HMuxTypography.micro.weight(.semibold))
                        Spacer()
                        Text("\(accounts.count)")
                            .font(HMuxTypography.micro.monospacedDigit())
                    }
                    ForEach(accounts.sorted { lhs, rhs in
                        if lhs.active != rhs.active { return lhs.active }
                        return lhs.label.localizedStandardCompare(rhs.label) == .orderedAscending
                    }) { account in
                        HMuxUsageAccountRow(account: account, stale: state.isStale)
                    }
                    if let updated = snapshot.accountsUpdatedAt {
                        Text("Accounts updated \(HMuxUsageFormat.reset(updated))")
                            .font(HMuxTypography.micro)
                            .foregroundStyle(Color.hmuxTertiaryText)
                    }
                } else if snapshot.isCodexPool {
                    Text("Account details are not available from Home yet.")
                        .font(HMuxTypography.micro)
                        .foregroundStyle(Color.hmuxTertiaryText)
                }
            }
        }
        .padding(12)
        .background(Color.hmuxRaised.opacity(0.58), in: RoundedRectangle(cornerRadius: 11))
        .overlay { RoundedRectangle(cornerRadius: 11).stroke(Color.hmuxBorder, lineWidth: 1) }
        .accessibilityElement(children: .contain)
    }

    private var stateChip: some View {
		Text(stateLabel)
            .font(.system(size: 9, weight: .semibold, design: .rounded))
            .foregroundStyle(state.hasUsableQuota && !state.isStale ? Color.hmuxConnected : Color.hmuxWaiting)
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(Color.hmuxWindow.opacity(0.8), in: Capsule())
	}

	private var stateLabel: String {
		guard state.phase == .connected else {
			return state.phase == .updateRequired ? "UPDATE REQUIRED" : state.phase.rawValue.uppercased()
		}
		if state.snapshot?.isClaudeSwap == true {
            return state.isStale ? "STALE" : (state.hasUsableQuota ? "ACTIVE ACCOUNT" : "NO ACTIVE QUOTA")
        }
        guard let status = state.snapshot?.status.state, status != "ok" else {
			return state.isStale ? "STALE" : (state.hasUsableQuota ? "CONNECTED" : "NO WEEKLY DATA")
		}
		switch status {
		case "networkError": return "NETWORK"
		case "authExpired", "codexLoggedOut": return "SIGN IN ON HOME"
		case "rateLimited": return "LIMITED"
		case "quotaEndpointChanged": return "CHANGED"
		default: return "UNAVAILABLE"
		}
	}

	private var sourceLabel: String? {
		if let snapshot = state.snapshot, snapshot.isClaudeSwap {
            return snapshot.activeClaudeAccount.map { "cswap · " + $0.label } ?? "cswap · No active account"
        }
        guard let source = state.snapshot?.status.quotaSource else { return nil }
		switch source {
		case "codex_lb": return "codex-lb · Entire account pool"
		case "oauth_api": return "\(provider.label) account"
		default: return nil
		}
	}

    private var unavailableMessage: String {
        if state.phase == .connected, let snapshot = state.snapshot, snapshot.isClaudeSwap {
            return snapshot.activeClaudeAccount == nil
                ? "HMux could not identify one active cswap account. Check the active account in cswap."
                : "No current weekly reading is cached for the active account. Open cswap to refresh its usage; HMux will pick it up automatically."
        }
        switch state.phase {
		case .connecting:
			return "HMux is connecting to the usage collector on your Home Mac."
		case .offline:
			return "The Home connection is reconnecting. Quota will update automatically."
		case .updateRequired:
			return "Update hmux-agent on the Home Mac to enable automatic usage reporting. No additional provider login is required."
		case .connected:
			switch state.snapshot?.status.state {
			case "networkError":
				return state.snapshot?.isCodexPool == true
                        ? "The codex-lb pool is temporarily unavailable. HMux will retry automatically."
                        : "The provider usage endpoint is temporarily unreachable. HMux will retry automatically."
			case "authExpired", "codexLoggedOut":
				return "No usable \(provider.label) session is currently available on Home. HMux uses the existing Home CLI session and requires no separate usage login."
			case "rateLimited":
				return retryMessage ?? "The provider usage endpoint is rate-limited. HMux will retry automatically."
			case "quotaEndpointChanged":
				return "The provider usage response changed and could not be read safely."
			default:
				return "No usable quota is available from this provider yet."
			}
		}
	}

    private var unavailableTitle: String {
        if state.phase == .connected, state.snapshot?.isClaudeSwap == true { return "Active account usage unavailable" }
        switch state.phase {
		case .connecting: return "Connecting to Home"
		case .offline: return "Usage reconnecting"
		case .updateRequired: return "Home agent update needed"
		case .connected:
			switch state.snapshot?.status.state {
			case "authExpired", "codexLoggedOut": return "Sign in on Home"
			case "networkError": return "Provider unavailable"
			case "rateLimited": return "Provider rate limited"
			case "quotaEndpointChanged": return "Usage format changed"
			default: return "Usage unavailable"
			}
		}
	}

	private var retryMessage: String? {
		guard let retryAt = state.snapshot?.status.retryDate else { return nil }
		let localized = retryAt.formatted(.dateTime.month(.abbreviated).day().hour().minute())
		return "Retry available after \(localized). Automatic checks run once a minute."
	}

    private func remainingText(_ snapshot: HMuxUsageSnapshot) -> String {
        HMuxTokenUsageStore.remainingPercent(snapshot: snapshot).map { "\($0)%" } ?? "—"
    }
}

private struct HMuxUsageAccountRow: View {
    let account: HMuxUsageAccount
    let stale: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Circle()
                    .fill(account.status == "ok" && !stale ? Color.hmuxConnected : Color.hmuxWaiting)
                    .frame(width: 5, height: 5)
                Text(account.label)
                    .font(HMuxTypography.caption.weight(.medium))
                    .foregroundStyle(Color.hmuxPrimaryText)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .help(account.label)
                Spacer(minLength: 4)
                if account.active {
                    Text("ACTIVE")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(Color.hmuxSelection)
                        .padding(.horizontal, 5).padding(.vertical, 2)
                        .background(Color.hmuxSelection.opacity(0.12), in: Capsule())
                }
                Text(account.status == "ok" ? "Ready" : HMuxUsageFormat.accountStatus(account.status))
                    .font(HMuxTypography.micro)
                    .foregroundStyle(Color.hmuxTertiaryText)
            }
            HMuxUsageWindowRow(window: HMuxUsageWindowDisplay(
                label: "1w", usedPct: account.sevenDay?.usedPct, resetsAt: account.sevenDay?.resetsAt
            ))
        }
        .padding(.vertical, 4)
        .opacity(stale ? 0.65 : 1)
        .accessibilityElement(children: .contain)
    }
}

private struct HMuxUsageWindowRow: View {
    let window: HMuxUsageWindowDisplay

    var body: some View {
        HStack(spacing: 8) {
            Text(window.label)
                .foregroundStyle(Color.hmuxSecondaryText)
                .frame(width: 22, alignment: .leading)
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.hmuxWindow)
                    Capsule()
                        .fill(barColor)
                        .frame(width: geometry.size.width * CGFloat(window.usedPct.map { 1 - $0 } ?? 0))
                }
            }
            .frame(height: 5)
            Text(remainingText)
                .font(HMuxTypography.micro.monospacedDigit())
                .foregroundStyle(Color.hmuxSecondaryText)
				.frame(width: 52, alignment: .trailing)
            Text(HMuxUsageFormat.reset(window.resetsAt))
                .font(HMuxTypography.micro.monospacedDigit())
                .foregroundStyle(Color.hmuxTertiaryText)
                .frame(width: 112, alignment: .trailing)
        }
        .font(HMuxTypography.micro)
        .accessibilityElement(children: .combine)
		.accessibilityLabel("\(window.label), \(remainingText), resets \(HMuxUsageFormat.reset(window.resetsAt))")
    }

    private var remainingText: String {
		window.usedPct.map { "\(Int((100 * (1 - $0)).rounded()))% left" } ?? "—"
    }

    private var barColor: Color {
        guard let used = window.usedPct else { return Color.hmuxTertiaryText }
        if used >= 0.85 { return Color.hmuxFailure }
        if used >= 0.65 { return Color.hmuxWaiting }
        return Color.hmuxSelection
    }
}

private enum HMuxUsageFormat {
    static func windows(for snapshot: HMuxUsageSnapshot) -> [HMuxUsageWindowDisplay] {
        snapshot.quotaWindows
    }

    static func reset(_ value: String?) -> String {
        guard let value, let date = isoDate(value) else { return "—" }
        return date.formatted(.dateTime.month(.abbreviated).day().hour().minute())
    }

    static func tokens(_ value: Int) -> String {
        if value >= 1_000_000 { return String(format: "%.1fM", Double(value) / 1_000_000) }
        if value >= 1_000 { return String(format: "%.1fK", Double(value) / 1_000) }
        return "\(value)"
    }

    static func accountStatus(_ value: String) -> String {
        switch value {
        case "stale": return "Cached"
        case "paused": return "Paused"
        case "deactivated": return "Inactive"
        case "reauth_required": return "Home session"
        case "rate_limited": return "Limited"
        case "token_expired": return "Expired"
        case "no_credentials": return "No credentials"
        case "unavailable": return "No quota data"
        default: return "Unavailable"
        }
    }

    private static func isoDate(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: value) ?? ISO8601DateFormatter().date(from: value)
    }
}

private struct HMuxRunningBedlView: View {
    let state: HMuxBedlBurnState
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var frameIndex = 0

    private static let order = [6, 5, 2, 1, 7, 8, 3, 8, 7, 1, 2, 5, 6, 4]

    var body: some View {
        HMuxBedlFrames.image(number: currentFrame)
			.renderingMode(.template)
            .resizable()
            .interpolation(.high)
            .scaledToFit()
				.foregroundStyle(Color.hmuxPrimaryText)
            .task(id: animationKey) { await animate() }
            .accessibilityHidden(true)
    }

    private var currentFrame: Int {
        guard !reduceMotion, state != .idle else { return 8 }
        return Self.order[frameIndex % Self.order.count]
    }

    private var animationKey: String { "\(state.rawValue)-\(reduceMotion)" }

    private func animate() async {
        await MainActor.run { frameIndex = 0 }
        guard !reduceMotion, let duration = state.cycleDuration else { return }
        let interval = max(0.05, duration / Double(Self.order.count))
        while !Task.isCancelled {
            do { try await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000)) }
            catch { return }
            await MainActor.run { frameIndex = (frameIndex + 1) % Self.order.count }
        }
    }
}

private enum HMuxBedlFrames {
    private static let images: [Int: NSImage] = {
        var result: [Int: NSImage] = [:]
        for number in 1...8 {
            guard let url = Bundle.main.url(
                forResource: "bedl-\(number)",
                withExtension: "png",
                subdirectory: "BedlFrames"
            ), let image = NSImage(contentsOf: url) else { continue }
			image.isTemplate = true
            result[number] = image
        }
        return result
    }()

    static func image(number: Int) -> Image {
        if let image = images[number] { return Image(nsImage: image) }
        return Image(systemName: "pawprint.fill")
    }
}

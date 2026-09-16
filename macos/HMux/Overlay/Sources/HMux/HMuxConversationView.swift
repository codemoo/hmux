import AppKit
import Combine
import SwiftUI

struct HMuxConversationView: View {
	let session: HMuxSession

	@State private var conversation: HMuxConversation?
	@State private var loadedIdentity: String?
	@State private var isInitialLoading = true
	@State private var isStale = false
	@State private var loadFailed = false
	@State private var showQuestions = false
	@State private var showCode = false
	@State private var query = ""
	@State private var isSearchFocused = false
	@State private var refreshGeneration = 0
	@State private var copiedMessageID: String?
	@State private var isApplicationActive = NSApp.isActive

	private var identity: HMuxSessionIdentity { HMuxSessionIdentity(session: session) }

	private var taskID: HMuxConversationTaskID {
		HMuxConversationTaskID(
			identity: session.identity,
			refreshGeneration: refreshGeneration,
			isApplicationActive: isApplicationActive
		)
	}

	private var currentConversation: HMuxConversation? {
		loadedIdentity == session.identity ? conversation : nil
	}

	private var displayedMessages: [HMuxConversationDisplayMessage] {
		guard let conversation = currentConversation, conversation.status == .ready else { return [] }
		return hmuxConversationDisplayMessages(
			conversation,
			showQuestions: showQuestions,
			showCode: showCode,
			query: query
		)
	}

	var body: some View {
		VStack(spacing: 0) {
			header
			Rectangle().fill(Color.hmuxBorder).frame(height: 1)
			content
		}
		.background(Color.hmuxWindow)
		.onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
			isApplicationActive = true
		}
		.onReceive(NotificationCenter.default.publisher(for: NSApplication.willResignActiveNotification)) { _ in
			isApplicationActive = false
		}
		.task(id: taskID) {
			guard isApplicationActive else { return }
			await poll(identity: identity, identityKey: session.identity)
		}
		.onDisappear {
			conversation = nil
			loadedIdentity = nil
			isInitialLoading = true
			isStale = false
			loadFailed = false
			copiedMessageID = nil
		}
	}

	private var header: some View {
		VStack(spacing: 10) {
			HStack(spacing: 10) {
				Image(systemName: "book.pages")
					.foregroundStyle(Color.hmuxSelection)
				VStack(alignment: .leading, spacing: 1) {
					Text("Conversation")
						.font(HMuxTypography.title)
					Text(session.displayName)
						.font(HMuxTypography.caption)
						.foregroundStyle(Color.hmuxSecondaryText)
						.lineLimit(1)
				}
				Spacer(minLength: 12)
				if isStale {
					Label("Earlier copy", systemImage: "exclamationmark.arrow.triangle.2.circlepath")
						.font(HMuxTypography.micro)
						.foregroundStyle(Color.hmuxWaiting)
						.help("The latest refresh failed. These are the last messages HMux loaded successfully.")
				}
				Menu {
					Toggle("Show questions", isOn: $showQuestions)
					Toggle("Show code blocks", isOn: $showCode)
				} label: {
					Image(systemName: "text.badge.checkmark")
						.frame(width: 26, height: 26)
				}
				.menuStyle(.borderlessButton)
				.fixedSize()
				.help("Conversation display options")
				.accessibilityLabel("Conversation display options")
				Button {
					refreshGeneration &+= 1
				} label: {
					Image(systemName: "arrow.clockwise")
						.frame(width: 26, height: 26)
				}
				.buttonStyle(.plain)
				.foregroundStyle(Color.hmuxSecondaryText)
				.help("Refresh conversation")
				.accessibilityLabel("Refresh conversation")
			}

			HStack(spacing: 7) {
				Image(systemName: "magnifyingglass")
					.font(.system(size: 11, weight: .medium))
					.foregroundStyle(Color.hmuxTertiaryText)
				HMuxSearchField(
					placeholder: "Search displayed messages",
					text: $query,
					isFocused: $isSearchFocused,
					onCancel: {
						if query.isEmpty { isSearchFocused = false }
						else { query = "" }
					}
				)
				.frame(maxWidth: .infinity, minHeight: 18, maxHeight: 18)
				Button {
					query = ""
				} label: {
					Image(systemName: "xmark.circle.fill")
						.frame(width: 16, height: 18)
				}
				.buttonStyle(.plain)
				.foregroundStyle(Color.hmuxTertiaryText)
				.opacity(query.isEmpty ? 0 : 1)
				.disabled(query.isEmpty)
				.accessibilityHidden(query.isEmpty)
				.accessibilityLabel("Clear conversation search")
			}
			.padding(.horizontal, 10)
			.frame(height: 30)
			.background(Color.hmuxRaised, in: RoundedRectangle(cornerRadius: 7))
			.overlay {
				RoundedRectangle(cornerRadius: 7).stroke(Color.hmuxStrongBorder.opacity(0.65), lineWidth: 1)
			}
		}
		.padding(.horizontal, 16)
		.padding(.vertical, 12)
		.background(Color.hmuxChrome)
	}

	@ViewBuilder
	private var content: some View {
		if loadedIdentity != session.identity || isInitialLoading {
			VStack(spacing: 12) {
				ProgressView().controlSize(.small)
				Text("Loading conversation…")
					.font(HMuxTypography.caption)
					.foregroundStyle(Color.hmuxSecondaryText)
			}
			.frame(maxWidth: .infinity, maxHeight: .infinity)
		} else if loadFailed, currentConversation == nil {
			placeholder(
				symbol: "exclamationmark.bubble",
				title: "Conversation unavailable",
				detail: "HMux couldn’t read this conversation. You can try again with the refresh button."
			)
		} else if let conversation = currentConversation {
			switch conversation.status {
			case .unavailable:
				placeholder(
					symbol: "text.bubble",
					title: "No conversation record",
					detail: "This session has no readable Codex conversation. Sessions created by older agents may not provide one."
				)
			case .ambiguous:
				placeholder(
					symbol: "questionmark.bubble",
					title: "Conversation match is ambiguous",
					detail: "More than one Codex record matches this session, so HMux won’t choose one automatically."
				)
			case .ready:
				conversationScroll(conversation)
			}
		} else {
			placeholder(
				symbol: "text.bubble",
				title: "No conversation record",
				detail: "This session has no readable Codex conversation."
			)
		}
	}

	private func conversationScroll(_ conversation: HMuxConversation) -> some View {
		ScrollViewReader { proxy in
			ZStack(alignment: .bottomTrailing) {
				ScrollView {
					HStack(alignment: .top) {
						Spacer(minLength: 20)
						LazyVStack(alignment: .leading, spacing: 14) {
							if conversation.truncated {
								Label("Showing the newest part of this conversation", systemImage: "ellipsis.circle")
									.font(HMuxTypography.caption)
									.foregroundStyle(Color.hmuxWaiting)
									.padding(.bottom, 2)
							}

							if displayedMessages.isEmpty {
								emptyReadyState(hasMessages: !conversation.messages.isEmpty)
							} else {
								ForEach(displayedMessages) { message in
									messageCard(message)
										.id(message.id)
								}
							}
						}
						.frame(maxWidth: 780, alignment: .leading)
						.padding(.vertical, 22)
						Spacer(minLength: 20)
					}
				}

                .onAppear {
                    if let lastID = displayedMessages.last?.id {
                        DispatchQueue.main.async { proxy.scrollTo(lastID, anchor: .bottom) }
                    }
                }

				if let lastID = displayedMessages.last?.id {
					Button {
						withAnimation(.easeOut(duration: 0.18)) {
							proxy.scrollTo(lastID, anchor: .bottom)
						}
					} label: {
						Label("Latest", systemImage: "arrow.down")
							.font(HMuxTypography.label)
					}
					.buttonStyle(.bordered)
					.controlSize(.small)
					.padding(14)
					.accessibilityHint("Scrolls to the last displayed message")
				}
			}
		}
	}

	private func messageCard(_ message: HMuxConversationDisplayMessage) -> some View {
		VStack(alignment: .leading, spacing: 10) {
			HStack(spacing: 7) {
				Image(systemName: message.role == .assistant ? "sparkles" : "person.fill")
					.font(.system(size: 10, weight: .semibold))
				Text(message.role == .assistant ? "ANSWER" : "QUESTION")
					.font(HMuxTypography.micro)
					.tracking(0.55)
				Spacer()
				Button {
					copy(message.text, messageID: message.id)
				} label: {
					Image(systemName: copiedMessageID == message.id ? "checkmark" : "doc.on.doc")
						.frame(width: 24, height: 22)
				}
				.buttonStyle(.plain)
				.disabled(message.text.isEmpty)
				.foregroundStyle(Color.hmuxSecondaryText)
				.help("Copy displayed message")
				.accessibilityLabel("Copy displayed message")
			}
			.foregroundStyle(message.role == .assistant ? Color.hmuxSelection : Color.hmuxSecondaryText)

			if message.text.isEmpty {
				Text("Code block hidden")
					.font(HMuxTypography.caption)
					.foregroundStyle(Color.hmuxTertiaryText)
			} else {
				Text(hmuxConversationInlineMarkdown(message.text))
					.font(.system(size: 14))
					.foregroundStyle(Color.hmuxPrimaryText)
					.lineSpacing(4)
					.fixedSize(horizontal: false, vertical: true)
					.textSelection(.enabled)
			}
		}
		.padding(.horizontal, 16)
		.padding(.vertical, 14)
		.background(Color.hmuxChrome.opacity(message.role == .assistant ? 0.86 : 0.58), in: RoundedRectangle(cornerRadius: 11))
		.overlay {
			RoundedRectangle(cornerRadius: 11).stroke(Color.hmuxBorder.opacity(0.8), lineWidth: 1)
		}
	}

	private func emptyReadyState(hasMessages: Bool) -> some View {
		VStack(spacing: 9) {
			Image(systemName: hasMessages ? "line.3.horizontal.decrease.circle" : "ellipsis.bubble")
				.font(.system(size: 22))
			Text(hasMessages ? "No displayed messages match" : "No conversation messages yet")
				.font(HMuxTypography.rowTitle)
			Text(hasMessages ? "Change the search or display options to see more." : "HMux will check again while this reader remains open.")
				.font(HMuxTypography.caption)
				.foregroundStyle(Color.hmuxSecondaryText)
		}
		.foregroundStyle(Color.hmuxPrimaryText)
		.frame(maxWidth: .infinity)
		.padding(.vertical, 64)
	}

	private func placeholder(symbol: String, title: String, detail: String) -> some View {
		VStack(spacing: 10) {
			Image(systemName: symbol)
				.font(.system(size: 26))
				.foregroundStyle(Color.hmuxTertiaryText)
			Text(title)
				.font(.system(size: 15, weight: .semibold))
			Text(detail)
				.font(HMuxTypography.body)
				.foregroundStyle(Color.hmuxSecondaryText)
				.multilineTextAlignment(.center)
				.fixedSize(horizontal: false, vertical: true)
				.frame(maxWidth: 430)
		}
		.frame(maxWidth: .infinity, maxHeight: .infinity)
		.padding(32)
	}

	@MainActor
	private func poll(identity: HMuxSessionIdentity, identityKey: String) async {
		if loadedIdentity != identityKey {
			conversation = nil
			loadedIdentity = identityKey
			isInitialLoading = true
			isStale = false
			loadFailed = false
			copiedMessageID = nil
		}

		while !Task.isCancelled {
			await refresh(identity: identity, identityKey: identityKey)
			do {
				try await Task.sleep(nanoseconds: 3_000_000_000)
			} catch {
				return
			}
		}
	}

	@MainActor
	private func refresh(identity: HMuxSessionIdentity, identityKey: String) async {
		do {
			let next = try await HMuxBackend.loadConversation(session: identity)
			try Task.checkCancellation()
			guard loadedIdentity == identityKey,
			      next.sessionID == identity.id,
			      next.createdAt == identity.createdAt else { return }
			conversation = next
			isStale = false
			loadFailed = false
			isInitialLoading = false
		} catch is CancellationError {
			return
		} catch {
			guard !Task.isCancelled, loadedIdentity == identityKey else { return }
			isInitialLoading = false
			if conversation == nil {
				loadFailed = true
			} else {
				isStale = true
			}
		}
	}

	private func copy(_ text: String, messageID: String) {
		let pasteboard = NSPasteboard.general
		pasteboard.clearContents()
		pasteboard.setString(text, forType: .string)
		copiedMessageID = messageID
	}
}

private struct HMuxConversationTaskID: Hashable {
	let identity: String
	let refreshGeneration: Int
	let isApplicationActive: Bool
}

private func hmuxConversationInlineMarkdown(_ source: String) -> AttributedString {
	let options = AttributedString.MarkdownParsingOptions(
		interpretedSyntax: .inlineOnlyPreservingWhitespace,
		failurePolicy: .returnPartiallyParsedIfPossible
	)
	guard var attributed = try? AttributedString(markdown: source, options: options) else {
		return AttributedString(source)
	}
	for run in attributed.runs where run.link != nil {
		attributed[run.range].link = nil
	}
	return attributed
}

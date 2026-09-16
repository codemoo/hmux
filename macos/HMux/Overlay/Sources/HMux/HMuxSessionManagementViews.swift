import AppKit
import SwiftUI

struct HMuxAliasEditorView: View {
    @ObservedObject var store: HMuxStore
    let session: HMuxSession
    @Environment(\.dismiss) private var dismiss
    @State private var alias: String
    @State private var errorMessage: String?
    @FocusState private var aliasFocused: Bool

    init(store: HMuxStore, session: HMuxSession) {
        self.store = store
        self.session = session
        _alias = State(initialValue: session.alias ?? "")
    }

    private var normalizedAlias: String {
        alias.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var isValid: Bool {
        normalizedAlias.utf8.count <= 128 && normalizedAlias.unicodeScalars.allSatisfy { scalar in
            !CharacterSet.controlCharacters.contains(scalar) && !isBidiControl(scalar.value)
        }
    }

	private var isUnchanged: Bool {
		hmuxCanonicalAlias(normalizedAlias) == hmuxCanonicalAlias(session.alias)
	}

	private var isBusy: Bool {
		store.isMutationPending(for: session)
	}

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 5) {
                Text("Edit Display Name")
                    .font(.system(size: 18, weight: .semibold))
                Text("The tmux session name remains “\(session.name)”. Leave this blank to restore it.")
                    .font(HMuxTypography.caption)
                    .foregroundStyle(Color.hmuxSecondaryText)
            }

            VStack(alignment: .leading, spacing: 6) {
                TextField("Display name", text: $alias)
                    .textFieldStyle(.roundedBorder)
                    .focused($aliasFocused)
                    .onSubmit(save)
					.disabled(isBusy)
				if !isValid {
					Text("Use a display name no longer than 128 UTF-8 bytes, without control or bidirectional formatting characters.")
						.font(HMuxTypography.micro)
						.foregroundStyle(Color.hmuxFailure)
				}
            }

            if let errorMessage {
                Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
                    .font(HMuxTypography.caption)
                    .foregroundStyle(Color.hmuxFailure)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
					.disabled(isBusy)
				Button(action: save) {
					HStack(spacing: 6) {
						if isBusy { ProgressView().controlSize(.small) }
						Text(isBusy ? "Saving…" : "Save")
					}
				}
                    .keyboardShortcut(.defaultAction)
					.disabled(!isValid || isUnchanged || isBusy)
            }
        }
        .padding(22)
        .frame(width: 440)
		.onAppear {
			store.setManagementInteraction("alias-\(session.identity)", active: true)
			aliasFocused = true
		}
		.onDisappear { store.setManagementInteraction("alias-\(session.identity)", active: false) }
		.interactiveDismissDisabled(isBusy)
    }

    private func save() {
		guard isValid, !isUnchanged, !isBusy else { return }
        errorMessage = nil
        store.setAlias(normalizedAlias, for: session) { succeeded, message in
            if !succeeded {
                errorMessage = message ?? "The display name could not be saved."
                aliasFocused = true
            }
        }
    }

    private func isBidiControl(_ value: UInt32) -> Bool {
        value == 0x061C || value == 0x200E || value == 0x200F ||
            (0x202A...0x202E).contains(value) || (0x2066...0x2069).contains(value)
    }
}

struct HMuxCreateSessionView: View {
	@ObservedObject var store: HMuxStore
	@Environment(\.dismiss) private var dismiss
	@State private var selectedProfileID = ""
	@State private var name = ""
	@State private var errorMessage: String?
	@FocusState private var nameFocused: Bool

	private var normalizedName: String {
		name.trimmingCharacters(in: .whitespacesAndNewlines)
	}

	private var isNameValid: Bool {
		normalizedName.isEmpty || hmuxIsValidSessionName(normalizedName)
	}

	private var selectedProfile: HMuxProfile? {
		store.availableProfiles.first { $0.id == selectedProfileID }
	}

	private var existingSession: HMuxSession? {
		guard !normalizedName.isEmpty else { return nil }
		return store.sessions.first { $0.name == normalizedName }
	}

	private var canOpenExistingWithoutNewTab: Bool {
		existingSession.map { store.openTabIDs.contains($0.identity) } ?? false
	}

	private var canSubmit: Bool {
        guard isNameValid, !store.isCreatingSession else { return false }
        if store.hasCreatedSessionToOpen { return selectedProfile != nil }
        if existingSession != nil { return canOpenExistingWithoutNewTab || store.canCreateSession }
        return selectedProfile != nil && store.canCreateSession
	}

	private var actionTitle: String {
		if store.isCreatingSession { return "Opening…" }
		if store.hasCreatedSessionToOpen { return "Retry Opening" }
		if existingSession != nil { return "Open Existing Session" }
		return "Create and Open"
	}

	var body: some View {
		VStack(alignment: .leading, spacing: 18) {
			VStack(alignment: .leading, spacing: 5) {
				Text("New Session")
					.font(.system(size: 18, weight: .semibold))
				Text("Choose a profile to start a terminal session on Home.")
					.font(HMuxTypography.caption)
					.foregroundStyle(Color.hmuxSecondaryText)
			}

			profileSelector

			VStack(alignment: .leading, spacing: 6) {
				Text("Session name (optional)")
					.font(HMuxTypography.caption)
				TextField("Automatic name", text: $name)
					.textFieldStyle(.roundedBorder)
					.focused($nameFocused)
					.onSubmit(create)
					.disabled(store.isCreatingSession || store.hasCreatedSessionToOpen)
				Text("An existing name opens that session. Leave blank to create a new one.")
					.font(HMuxTypography.micro)
					.foregroundStyle(Color.hmuxTertiaryText)
				if !isNameValid {
					Text("Use at most 80 letters, numbers, spaces, underscores, or hyphens.")
						.font(HMuxTypography.micro)
						.foregroundStyle(Color.hmuxFailure)
				}
			}

			if let existingSession, !store.hasCreatedSessionToOpen {
				existingSessionCard(existingSession)
			}

			if let errorMessage {
				Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
					.font(HMuxTypography.caption)
					.foregroundStyle(Color.hmuxFailure)
					.fixedSize(horizontal: false, vertical: true)
			}
			if store.hasCreatedSessionToOpen && !store.isCreatingSession {
				Text("The session is already on Home. Retry opens that same session.")
					.font(HMuxTypography.caption)
					.foregroundStyle(Color.hmuxSecondaryText)
			} else if let reason = store.createSessionUnavailableReason,
			          selectedProfile != nil,
			          !store.canCreateSession,
			          !canOpenExistingWithoutNewTab {
				Label(reason, systemImage: "info.circle")
					.font(HMuxTypography.caption)
					.foregroundStyle(Color.hmuxSecondaryText)
					.fixedSize(horizontal: false, vertical: true)
			}

			HStack {
				Spacer()
				Button("Cancel", role: .cancel) { dismiss() }
					.keyboardShortcut(.cancelAction)
					.disabled(store.isCreatingSession)
				Button(action: create) {
					HStack(spacing: 6) {
						if store.isCreatingSession { ProgressView().controlSize(.small) }
						Text(actionTitle)
					}
				}
					.keyboardShortcut(.defaultAction)
					.disabled(!canSubmit)
			}
		}
		.padding(22)
		.frame(width: 470)
		.onAppear {
			store.setManagementInteraction("create-session", active: true)
			store.loadProfiles()
			selectFirstProfileIfNeeded()
			nameFocused = true
		}
		.onChange(of: store.availableProfiles.map(\.id)) { _ in
			selectFirstProfileIfNeeded()
		}
		.onDisappear { store.setManagementInteraction("create-session", active: false) }
		.interactiveDismissDisabled(store.isCreatingSession)
	}

	@ViewBuilder
	private var profileSelector: some View {
		VStack(alignment: .leading, spacing: 7) {
			Text("Profile")
				.font(HMuxTypography.caption)
			if store.isLoadingProfiles && store.availableProfiles.isEmpty {
				HStack(spacing: 8) {
					ProgressView().controlSize(.small)
					Text("Loading Home profiles…")
				}
				.font(HMuxTypography.caption)
				.foregroundStyle(Color.hmuxSecondaryText)
				.frame(maxWidth: .infinity, alignment: .leading)
				.padding(.vertical, 7)
			} else if let message = store.profileLoadErrorMessage, store.availableProfiles.isEmpty {
				VStack(alignment: .leading, spacing: 8) {
					Label(message, systemImage: "exclamationmark.triangle.fill")
						.foregroundStyle(Color.hmuxFailure)
					Button("Try Again") { store.loadProfiles(force: true) }
						.buttonStyle(.bordered)
						.controlSize(.small)
				}
				.font(HMuxTypography.caption)
			} else if store.availableProfiles.isEmpty {
				VStack(alignment: .leading, spacing: 8) {
					Label("No session profiles are available from Home.", systemImage: "tray")
						.foregroundStyle(Color.hmuxSecondaryText)
					Button("Try Again") { store.loadProfiles(force: true) }
						.buttonStyle(.bordered)
						.controlSize(.small)
				}
				.font(HMuxTypography.caption)
			} else {
				Picker("Profile", selection: $selectedProfileID) {
					ForEach(store.availableProfiles) { profile in
						Text("\(profile.label) · \(profile.id)").tag(profile.id)
					}
				}
				.labelsHidden()
					.pickerStyle(.menu)
					.disabled(store.isCreatingSession || store.hasCreatedSessionToOpen)
				.frame(maxWidth: .infinity, alignment: .leading)
				if let selectedProfile, !selectedProfile.tags.isEmpty {
					Text(selectedProfile.tags.joined(separator: " · "))
						.font(HMuxTypography.micro)
						.foregroundStyle(Color.hmuxTertiaryText)
				}
			}
		}
	}

	private func existingSessionCard(_ session: HMuxSession) -> some View {
		VStack(alignment: .leading, spacing: 7) {
			Label("Existing Home session", systemImage: "arrow.turn.down.right")
				.font(HMuxTypography.caption.weight(.semibold))
				.foregroundStyle(Color.hmuxSelection)
			Text(session.displayName)
				.font(HMuxTypography.rowTitle)
			HStack(spacing: 5) {
				Text("Session \(session.name)")
				Text("·")
				Text(session.hmuxRuntimeLabel)
				Text("·")
				Text(session.hmuxProjectName)
			}
			.font(HMuxTypography.caption)
			.foregroundStyle(Color.hmuxSecondaryText)
			.lineLimit(1)
            if let profile = session.profile {
                Text("Profile: \(profile)").font(HMuxTypography.micro)
                    .foregroundStyle(Color.hmuxTertiaryText)
            }
			Text("Its existing profile and work will be preserved.")
				.font(HMuxTypography.micro)
				.foregroundStyle(Color.hmuxTertiaryText)
		}
		.padding(11)
		.frame(maxWidth: .infinity, alignment: .leading)
		.background(Color.hmuxRaised.opacity(0.65), in: RoundedRectangle(cornerRadius: 9))
		.overlay { RoundedRectangle(cornerRadius: 9).stroke(Color.hmuxBorder, lineWidth: 1) }
	}

	private func selectFirstProfileIfNeeded() {
		guard !store.availableProfiles.contains(where: { $0.id == selectedProfileID }) else { return }
		selectedProfileID = store.availableProfiles.first?.id ?? ""
	}

	private func create() {
		guard canSubmit else { return }
        errorMessage = nil
        if let existingSession, !store.hasCreatedSessionToOpen {
            if store.open(existingSession) { dismiss() }
            else {
                errorMessage = store.actionErrorMessage ?? "This session could not be opened."
                store.dismissActionError()
            }
            return
        }
		store.createSession(profileID: selectedProfileID, name: normalizedName) { succeeded, message in
			if succeeded {
				dismiss()
			} else {
				errorMessage = message ?? "The session could not be created."
				nameFocused = true
			}
		}
	}
}

struct HMuxHiddenSessionsView: View {
    @ObservedObject var store: HMuxStore
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var mutationError: String?
    @State private var terminationSession: HMuxSession?
	@State private var restoringSessionIDs = Set<String>()
	@FocusState private var searchFocused: Bool

    private var rows: [HMuxSessionRowState] {
        store.hiddenRows(matching: query)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Hidden Sessions")
                        .font(.system(size: 18, weight: .semibold))
                    Text("Hidden sessions keep running and remain available in already-open tabs.")
                        .font(HMuxTypography.caption)
                        .foregroundStyle(Color.hmuxSecondaryText)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)
					.disabled(!restoringSessionIDs.isEmpty)
            }
            .padding(18)

            Rectangle().fill(Color.hmuxBorder).frame(height: 1)

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(Color.hmuxSecondaryText)
                TextField("Search hidden sessions", text: $query)
                    .textFieldStyle(.plain)
					.focused($searchFocused)
					.disabled(!restoringSessionIDs.isEmpty)
				if !query.isEmpty {
					Button { query = "" } label: {
						Image(systemName: "xmark.circle.fill")
					}
					.buttonStyle(.plain)
					.foregroundStyle(Color.hmuxTertiaryText)
					.disabled(!restoringSessionIDs.isEmpty)
					.accessibilityLabel("Clear search")
				}
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .background(Color.hmuxRaised, in: RoundedRectangle(cornerRadius: 8))
            .padding(12)

            if let mutationError {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                    Text(mutationError).lineLimit(3)
                    Spacer()
                    Button("Dismiss") { self.mutationError = nil }
                        .buttonStyle(.link)
                }
                .font(HMuxTypography.caption)
                .foregroundStyle(Color.hmuxFailure)
                .padding(.horizontal, 14)
                .padding(.bottom, 10)
                .accessibilityElement(children: .contain)
            }

            if rows.isEmpty {
                VStack(spacing: 9) {
                    Image(systemName: "eye.slash")
                        .font(.system(size: 25, weight: .light))
                    Text(query.isEmpty ? "No hidden sessions" : "No matching hidden sessions")
                        .font(HMuxTypography.rowTitle)
                }
                .foregroundStyle(Color.hmuxSecondaryText)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(rows) { row in
					HMuxHiddenSessionRow(
						store: store,
						row: row,
						isRestoring: restoringSessionIDs.contains(row.id),
						onRestore: { restore(row.session) },
						onTerminate: { terminationSession = $0 }
					)
                    .listRowBackground(Color.hmuxChrome)
                }
                .listStyle(.inset)
                .scrollContentBackground(.hidden)
            }
        }
		.frame(minWidth: 600, minHeight: 420)
        .sheet(item: $terminationSession) { session in
            HMuxTerminateSessionView(store: store, session: session)
                    .background(Color.hmuxChrome)
                    .hmuxTheme()
        }
		.onAppear { searchFocused = true }
		.interactiveDismissDisabled(!restoringSessionIDs.isEmpty)
    }

	private func restore(_ session: HMuxSession) {
		guard restoringSessionIDs.insert(session.identity).inserted else { return }
		mutationError = nil
		store.setHidden(false, for: session) { succeeded, message in
			restoringSessionIDs.remove(session.identity)
			if !succeeded {
				mutationError = message ?? "The hidden session could not be restored."
			}
		}
	}
}

private struct HMuxHiddenSessionRow: View {
	@ObservedObject var store: HMuxStore
	@ObservedObject var row: HMuxSessionRowState
	let isRestoring: Bool
	let onRestore: () -> Void
	let onTerminate: (HMuxSession) -> Void

	var body: some View {
		HStack(spacing: 12) {
			Image(systemName: row.session.hmuxStateSymbol)
				.foregroundStyle(row.session.hmuxStateColor)
				.frame(width: 18)
			VStack(alignment: .leading, spacing: 3) {
				Text(row.session.displayName)
					.font(HMuxTypography.rowTitle)
				HStack(spacing: 5) {
					if row.session.displayName != row.session.name {
						Text(row.session.name)
						Text("·")
					}
					Text(row.session.hmuxProjectName)
					Text("·")
					Text(row.session.hmuxActivityDate, style: .relative)
				}
				.font(HMuxTypography.caption)
				.foregroundStyle(Color.hmuxSecondaryText)
				.lineLimit(1)
			}
			Spacer()
			Button(action: onRestore) {
				HStack(spacing: 5) {
					if isRestoring { ProgressView().controlSize(.small) }
					Text(isRestoring ? "Restoring…" : "Restore")
				}
			}
			.buttonStyle(.bordered)
			.controlSize(.small)
			.disabled(isRestoring || store.isMutationPending(for: row.session))
			.accessibilityLabel("Restore \(row.session.displayName)")
			Menu {
				Button("Terminate Session…", role: .destructive) {
					onTerminate(row.session)
				}
			} label: {
				Image(systemName: "ellipsis")
					.frame(width: 24, height: 24)
			}
			.menuStyle(.borderlessButton)
			.menuIndicator(.hidden)
			.fixedSize()
			.disabled(store.isMutationPending(for: row.session))
			.accessibilityLabel("Manage \(row.session.displayName)")
		}
		.padding(.vertical, 4)
	}
}

struct HMuxTerminateSessionView: View {
    @ObservedObject var store: HMuxStore
    let session: HMuxSession
    @Environment(\.dismiss) private var dismiss
    @State private var confirmation = ""
    @State private var errorMessage: String?
    @State private var isTerminating = false
    @FocusState private var confirmationFocused: Bool

	private var isConfirmed: Bool { confirmation == session.name }
	private var isBusy: Bool { isTerminating || store.isMutationPending(for: session) }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(Color.hmuxFailure)
                Text("Terminate Session")
                    .font(.system(size: 18, weight: .semibold))
            }
            Text("This ends the tmux session and every process running inside it. This is different from closing an HMux visual tab.")
                .font(HMuxTypography.body)
                .foregroundStyle(Color.hmuxSecondaryText)
                .fixedSize(horizontal: false, vertical: true)

			targetCard

            VStack(alignment: .leading, spacing: 7) {
				Text("Type “\(session.name)” to confirm.")
                    .font(HMuxTypography.caption)
				TextField(session.name, text: $confirmation)
                    .textFieldStyle(.roundedBorder)
                    .focused($confirmationFocused)
                    .onSubmit(terminate)
					.disabled(isBusy)
            }

            if let errorMessage {
                Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
                    .font(HMuxTypography.caption)
                    .foregroundStyle(Color.hmuxFailure)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
					.disabled(isBusy)
				Button(role: .destructive, action: terminate) {
					HStack(spacing: 6) {
						if isBusy { ProgressView().controlSize(.small) }
						Text(isBusy ? "Terminating…" : "Terminate Session")
					}
				}
					.disabled(!isConfirmed || isBusy)
            }
        }
        .padding(22)
        .frame(width: 460)
		.onAppear {
			store.setManagementInteraction("terminate-\(session.identity)", active: true)
			confirmationFocused = true
		}
		.onDisappear { store.setManagementInteraction("terminate-\(session.identity)", active: false) }
		.interactiveDismissDisabled(isBusy)
    }

	private var targetCard: some View {
		VStack(alignment: .leading, spacing: 7) {
			Text("Session to end")
				.font(HMuxTypography.micro)
				.foregroundStyle(Color.hmuxTertiaryText)
			if session.displayName != session.name {
				HStack(alignment: .firstTextBaseline) {
					Text(session.displayName)
						.font(HMuxTypography.rowTitle)
					Spacer()
					Text("Display name")
						.font(HMuxTypography.micro)
						.foregroundStyle(Color.hmuxTertiaryText)
				}
			}
			HStack(spacing: 5) {
				Text(session.name)
					.font(HMuxTypography.caption.weight(.semibold))
				Text("·")
				Text(session.hmuxRuntimeLabel)
				Text("·")
				Text(session.hmuxProjectName)
			}
			.foregroundStyle(Color.hmuxSecondaryText)
			.lineLimit(1)
            if !session.currentPath.isEmpty {
                Text(session.currentPath)
                    .font(HMuxTypography.caption).foregroundStyle(Color.hmuxSecondaryText)
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
		}
		.padding(11)
		.background(Color.hmuxRaised.opacity(0.65), in: RoundedRectangle(cornerRadius: 9))
		.overlay { RoundedRectangle(cornerRadius: 9).stroke(Color.hmuxBorder, lineWidth: 1) }
	}

    private func terminate() {
		guard isConfirmed, !isBusy else { return }
        isTerminating = true
        errorMessage = nil
        store.terminate(session) { succeeded, message in
            isTerminating = false
            if succeeded {
                dismiss()
            } else {
                errorMessage = message ?? "The session could not be terminated."
                confirmationFocused = true
            }
        }
    }
}

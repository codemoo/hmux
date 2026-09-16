import SwiftUI

struct HMuxInspectorView: View {
    @ObservedObject var tab: HMuxTerminalTab
    let onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            inspectorHeader
            Rectangle().fill(Color.hmuxBorder).frame(height: 1)
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
					VStack(alignment: .leading, spacing: 5) {
						Text(tab.session.displayName)
							.font(HMuxTypography.title)
							.foregroundStyle(Color.hmuxPrimaryText)
							.textSelection(.enabled)
						if tab.session.displayName != tab.session.name {
							Text(tab.session.name).font(HMuxTypography.caption)
								.foregroundStyle(Color.hmuxSecondaryText).textSelection(.enabled)
						}
						if !tab.session.currentPath.isEmpty {
							Text(tab.session.currentPath).font(HMuxTypography.caption)
								.foregroundStyle(Color.hmuxSecondaryText).textSelection(.enabled)
								.fixedSize(horizontal: false, vertical: true)
						}
					}
                    workflowSummary
                    workflows
                    sessionDetails
                }
                .padding(13)
            }
        }
        .background(Color.hmuxSidebar)
    }

    private var inspectorHeader: some View {
        HStack(spacing: 8) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .foregroundStyle(Color.hmuxSelection)
            Text("Session Details")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.hmuxPrimaryText)
            Spacer()
            HMuxInspectorStatePill(session: tab.session)
            Button(action: onClose) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .frame(width: 24, height: 24)
            }
            .buttonStyle(.plain)
            .foregroundStyle(Color.hmuxSecondaryText)
            .help("Close inspector (⌥⌘I)")
            .accessibilityLabel("Close session details")
        }
        .padding(.horizontal, 13)
        .frame(height: 42)
    }

    @ViewBuilder
    private var workflowSummary: some View {
        if let summary = tab.session.workflow {
            VStack(alignment: .leading, spacing: 9) {
                HMuxInspectorSectionTitle(title: "OVERVIEW", count: nil)
                LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 7) {
                    HMuxSummaryCell(label: "Running", value: summary.running, color: .hmuxConnected)
                    HMuxSummaryCell(label: "Waiting", value: summary.waitingApproval + summary.waitingInput, color: .hmuxWaiting)
                    HMuxSummaryCell(label: "Complete", value: summary.completed, color: .hmuxSelection)
                    HMuxSummaryCell(label: "Failed", value: summary.failed, color: .hmuxFailure)
                }
            }
        }
    }

    @ViewBuilder
    private var workflows: some View {
        let items = tab.session.workflows ?? []
        VStack(alignment: .leading, spacing: 9) {
            HMuxInspectorSectionTitle(title: "AGENT TREE", count: items.count)
            if items.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "point.3.connected.trianglepath.dotted")
                        .font(.title3)
                    Text("No active workflow")
                        .font(.caption.weight(.medium))
                    Text("Sessions with workflow reporting show their agent activity here.")
                        .font(.caption2)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, 10)
                }
                .foregroundStyle(Color.hmuxSecondaryText)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 18)
                .background(Color.hmuxSurface.opacity(0.55), in: RoundedRectangle(cornerRadius: HMuxLayout.panelRadius))
            } else {
                ForEach(items) { workflow in
                    HMuxWorkflowCard(workflow: workflow)
                }
            }
        }
    }

    private var sessionDetails: some View {
        VStack(alignment: .leading, spacing: 9) {
            HMuxInspectorSectionTitle(title: "SESSION", count: nil)
            VStack(spacing: 0) {
                HMuxMetadataRow(label: "Runtime", value: tab.session.hmuxRuntimeLabel)
                if let model = tab.session.modelLabel {
                    HMuxMetadataRow(label: "Model", value: model)
                }
                HMuxMetadataRow(label: "Command", value: tab.session.detailCommand.isEmpty ? "shell" : tab.session.detailCommand)
                HMuxMetadataRow(label: "Windows", value: "\(tab.session.windowCount)")
                HMuxMetadataRow(label: "Clients", value: "\(tab.session.attachedClients)")
                HMuxMetadataRow(label: "Created", value: Date(timeIntervalSince1970: TimeInterval(tab.session.createdAt)).formatted(date: .abbreviated, time: .shortened))
                if let tags = tab.session.tags, !tags.isEmpty {
                    HMuxMetadataRow(label: "Tags", value: tags.joined(separator: " · "), isLast: true)
                } else {
                    HMuxMetadataRow(label: "tmux ID", value: tab.session.id, isLast: true)
                }
            }
            .background(Color.hmuxSurface.opacity(0.62), in: RoundedRectangle(cornerRadius: HMuxLayout.panelRadius))
        }
    }
}

private struct HMuxInspectorStatePill: View {
    let session: HMuxSession

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(session.hmuxStateColor).frame(width: 5, height: 5)
            Text(session.hmuxStateLabel)
        }
        .font(HMuxTypography.micro)
        .foregroundStyle(session.hmuxStateColor)
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(session.hmuxStateColor.opacity(0.12), in: Capsule())
        .accessibilityElement(children: .combine)
    }
}

private struct HMuxInspectorSectionTitle: View {
    let title: String
    let count: Int?

    var body: some View {
        HStack {
            Text(title)
            Spacer()
            if let count { Text("\(count)").monospacedDigit() }
        }
        .font(HMuxTypography.micro)
        .tracking(0.6)
        .foregroundStyle(Color.hmuxTertiaryText)
    }
}

private struct HMuxSummaryCell: View {
    let label: String
    let value: Int
    let color: Color

    var body: some View {
        HStack(spacing: 8) {
            Circle().fill(color).frame(width: 7, height: 7)
            VStack(alignment: .leading, spacing: 1) {
                Text("\(value)")
                    .font(.system(size: 14, weight: .semibold, design: .rounded))
                    .monospacedDigit()
                    .foregroundStyle(Color.hmuxPrimaryText)
                Text(label)
                    .font(HMuxTypography.micro)
                    .foregroundStyle(Color.hmuxSecondaryText)
            }
            Spacer(minLength: 0)
        }
        .padding(9)
        .background(Color.hmuxSurface.opacity(0.65), in: RoundedRectangle(cornerRadius: 9))
        .accessibilityElement(children: .combine)
    }
}

private struct HMuxWorkflowCard: View {
    let workflow: HMuxWorkflow
	private let nodeDepths: [String: Int]

	init(workflow: HMuxWorkflow) {
		self.workflow = workflow
		nodeDepths = hmuxWorkflowNodeDepths(workflow.nodes)
	}

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 7) {
                Image(systemName: hmuxStatusSymbol(workflow.status))
                    .foregroundStyle(hmuxStatusColor(workflow.status))
                Text(workflow.source)
                    .font(HMuxTypography.label)
                    .foregroundStyle(Color.hmuxPrimaryText)
                    .lineLimit(1)
                Spacer()
                Text(workflow.status.replacingOccurrences(of: "_", with: " ").capitalized)
                    .font(HMuxTypography.micro)
                    .foregroundStyle(hmuxStatusColor(workflow.status))
            }

            if let model = workflow.model, !model.isEmpty {
                Label(model, systemImage: "cpu")
                    .font(HMuxTypography.micro)
                    .foregroundStyle(Color.hmuxTertiaryText)
            }

            ForEach(workflow.nodes) { node in
				HMuxWorkflowNodeRow(node: node, depth: nodeDepths[node.id] ?? 0)
            }
        }
        .padding(10)
        .background(Color.hmuxSurface.opacity(0.65), in: RoundedRectangle(cornerRadius: HMuxLayout.panelRadius))
        .overlay {
            RoundedRectangle(cornerRadius: HMuxLayout.panelRadius)
                .stroke(Color.hmuxBorder.opacity(0.72), lineWidth: 1)
        }
    }

}

private struct HMuxWorkflowNodeRow: View {
    let node: HMuxWorkflowNode
    let depth: Int

    var body: some View {
        HStack(spacing: 7) {
            if depth > 0 {
                Rectangle()
                    .fill(Color.hmuxBorder)
                    .frame(width: 1, height: 24)
                    .padding(.leading, CGFloat((depth - 1) * 12 + 5))
            }
            Image(systemName: hmuxStatusSymbol(node.status))
                .font(.system(size: 11))
                .foregroundStyle(hmuxStatusColor(node.status))
            VStack(alignment: .leading, spacing: 1) {
                Text(node.type.replacingOccurrences(of: "_", with: " ").capitalized)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Color.hmuxPrimaryText)
                Text(node.provider)
                    .font(HMuxTypography.micro)
                    .foregroundStyle(Color.hmuxTertiaryText)
            }
            Spacer(minLength: 0)
            Text(hmuxCompactDuration(startedAt: node.startedAt, endedAt: node.endedAt))
                .font(HMuxTypography.micro.monospacedDigit())
                .foregroundStyle(Color.hmuxTertiaryText)
        }
        .padding(.leading, CGFloat(depth * 4))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Level \(depth + 1), \(node.type), \(node.provider), \(node.status)")
    }
}

private struct HMuxMetadataRow: View {
    let label: String
    let value: String
    var isLast = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(label)
                .foregroundStyle(Color.hmuxSecondaryText)
            Spacer(minLength: 4)
            Text(value)
                .foregroundStyle(Color.hmuxPrimaryText)
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
        }
        .font(HMuxTypography.caption)
        .padding(.horizontal, 10)
        .frame(minHeight: 31)
        .overlay(alignment: .bottom) {
            if !isLast { Rectangle().fill(Color.hmuxBorder.opacity(0.65)).frame(height: 1) }
        }
        .accessibilityElement(children: .combine)
    }
}

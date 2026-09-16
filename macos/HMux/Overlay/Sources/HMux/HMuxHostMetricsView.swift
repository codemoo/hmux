import Foundation
import SwiftUI

struct HMuxHostMetricsView: View {
	@ObservedObject private var store: HMuxHostMetricsStore
	@State private var showsDetails = false

	init(store: HMuxHostMetricsStore) {
		_store = ObservedObject(wrappedValue: store)
	}

	var body: some View {
		Button {
			showsDetails.toggle()
		} label: {
			HStack(spacing: 7) {
				Image(systemName: "desktopcomputer")
					.font(.system(size: 10, weight: .semibold))
				Text("Home").font(.system(size: 10, weight: .semibold))
				HMuxCompactHostMetric(label: "CPU", value: percentText(metrics?.cpuPercent), width: 58)
				HMuxCompactHostMetric(label: "GPU", value: percentText(metrics?.gpuPercent), width: 58)
				HMuxCompactHostMetric(label: "RAM", value: memoryPercentText(metrics), width: 58)
				Image(systemName: "clock.badge.exclamationmark")
					.font(.system(size: 8, weight: .medium))
					.foregroundStyle(Color.hmuxWaiting)
					.opacity(showsWarning ? 1 : 0)
					.accessibilityHidden(true)
			}
			.padding(.horizontal, 8)
			.frame(height: 22)
			.contentShape(RoundedRectangle(cornerRadius: 7))
		}
		.buttonStyle(.plain)
		.foregroundStyle(showsDetails ? Color.hmuxSelection : Color.hmuxSecondaryText)
		.background(Color.hmuxRaised.opacity(showsDetails ? 1 : 0.72), in: RoundedRectangle(cornerRadius: 7))
		.overlay { RoundedRectangle(cornerRadius: 7).stroke(Color.hmuxBorder.opacity(0.7), lineWidth: 1) }
		.help("Home server CPU, GPU, and memory")
		.accessibilityLabel(accessibilityLabel)
		.popover(isPresented: $showsDetails, arrowEdge: .bottom) {
			HMuxHostMetricsDetailsView(presentation: store.presentation)
				.hmuxTheme()
		}
	}

	private var metrics: HMuxHostMetrics? {
		guard store.presentation.availability == .available else { return nil }
		return store.presentation.metrics
	}

	private var showsWarning: Bool {
		switch store.presentation.availability {
		case .unavailable(.stale), .unavailable(.clockUncertain): return true
		default: return false
		}
	}

	private var accessibilityLabel: String {
		guard let metrics else { return "Home server metrics, \(availabilityText(store.presentation.availability))" }
		let cpu = metrics.cpuPercent.map { "\(percentText($0)) CPU" } ?? "CPU unavailable"
		let gpu = metrics.gpuPercent.map { "\(percentText($0)) GPU" } ?? "GPU unavailable"
		return "Home server metrics, \(cpu), \(gpu), memory \(memoryText(metrics))"
	}
}

private struct HMuxCompactHostMetric: View {
	let label: String
	let value: String
	let width: CGFloat

	var body: some View {
		HStack(spacing: 3) {
			Text(label).foregroundStyle(Color.hmuxSecondaryText)
			Text(value)
				.font(.system(size: 10, weight: .semibold, design: .rounded).monospacedDigit())
				.foregroundStyle(Color.hmuxPrimaryText)
		}
		.frame(width: width, alignment: .leading)
	}
}

private struct HMuxHostMetricsDetailsView: View {
	let presentation: HMuxHostMetricsPresentation

	var body: some View {
		VStack(alignment: .leading, spacing: 14) {
			HStack(spacing: 10) {
				ZStack {
					RoundedRectangle(cornerRadius: 9).fill(Color.hmuxSelection.opacity(0.14))
					Image(systemName: "desktopcomputer")
						.foregroundStyle(Color.hmuxSelection)
				}
				.frame(width: 34, height: 34)
				VStack(alignment: .leading, spacing: 2) {
					Text("Home server")
						.font(.system(size: 14, weight: .semibold))
						.foregroundStyle(Color.hmuxPrimaryText)
					Text("Shared load from the Mac that runs all HMux tmux sessions.")
						.font(HMuxTypography.caption)
						.foregroundStyle(Color.hmuxSecondaryText)
				}
			}

			if let metrics = availableMetrics {
				VStack(spacing: 8) {
					HMuxHostMetricDetailRow(
						label: "CPU",
						value: metrics.cpuPercent.map(percentText) ?? "Unavailable",
						detail: "Overall processor utilization"
					)
					HMuxHostMetricDetailRow(
						label: "GPU",
						value: metrics.gpuPercent.map(percentText) ?? "Unavailable",
						detail: metrics.gpuPercent == nil
							? "GPU utilization is unavailable when macOS cannot provide a sample."
							: "Graphics processor utilization"
					)
					HMuxHostMetricDetailRow(
						label: "Memory",
						value: memoryText(metrics),
						detail: memoryDetail(metrics)
					)
				}

				HStack(spacing: 5) {
					Circle().fill(Color.hmuxConnected).frame(width: 6, height: 6)
					Text("Updated")
					Text(metrics.observedDate, style: .relative)
				}
				.font(HMuxTypography.micro.monospacedDigit())
				.foregroundStyle(Color.hmuxTertiaryText)
			} else {
				HStack(alignment: .top, spacing: 8) {
					Image(systemName: warningSymbol)
						.foregroundStyle(warningColor)
					Text(unavailableExplanation)
						.font(HMuxTypography.caption)
						.foregroundStyle(Color.hmuxSecondaryText)
						.fixedSize(horizontal: false, vertical: true)
				}
			}
		}
		.padding(16)
		.frame(width: 340)
		.background(Color.hmuxSurface)
	}

	private var availableMetrics: HMuxHostMetrics? {
		guard presentation.availability == .available else { return nil }
		return presentation.metrics
	}

	private var warningSymbol: String {
		switch presentation.availability {
		case .unavailable(.stale), .unavailable(.clockUncertain): return "clock.badge.exclamationmark"
		case .unavailable(.offline): return "network.slash"
		default: return "gauge.with.dots.needle.0percent"
		}
	}

	private var warningColor: Color {
		switch presentation.availability {
		case .unavailable(.stale), .unavailable(.clockUncertain): return Color.hmuxWaiting
		default: return Color.hmuxTertiaryText
		}
	}

	private var unavailableExplanation: String {
		switch presentation.availability {
		case .waiting:
			return "Waiting for the first Home server observation."
		case .unavailable(.noObservation):
			return "Host metrics are unavailable. The Home agent may need an update, or macOS could not produce a valid observation."
		case .unavailable(.stale):
			return "The last Home server observation is stale, so HMux has hidden its values."
		case .unavailable(.offline):
			return "The Home catalog is offline. Metrics will return after it reconnects."
		case .unavailable(.clockUncertain):
			return "The Home and client clocks disagree, so HMux cannot determine whether this observation is current."
		case .available:
			return "Home server metrics are available."
		}
	}
}

private struct HMuxHostMetricDetailRow: View {
	let label: String
	let value: String
	let detail: String

	var body: some View {
		HStack(alignment: .firstTextBaseline, spacing: 10) {
			VStack(alignment: .leading, spacing: 2) {
				Text(label)
					.font(HMuxTypography.label)
					.foregroundStyle(Color.hmuxPrimaryText)
				Text(detail)
					.font(HMuxTypography.micro)
					.foregroundStyle(Color.hmuxTertiaryText)
			}
			Spacer(minLength: 8)
			Text(value)
				.font(.system(size: 12, weight: .semibold, design: .rounded).monospacedDigit())
				.foregroundStyle(Color.hmuxPrimaryText)
		}
		.padding(10)
		.background(Color.hmuxRaised.opacity(0.58), in: RoundedRectangle(cornerRadius: 9))
		.overlay { RoundedRectangle(cornerRadius: 9).stroke(Color.hmuxBorder, lineWidth: 1) }
	}
}

private func availabilityText(_ availability: HMuxHostMetricsAvailability) -> String {
	switch availability {
	case .waiting: return "waiting"
	case .available: return "available"
	case .unavailable(.noObservation): return "unavailable"
	case .unavailable(.stale): return "stale"
	case .unavailable(.offline): return "Home catalog offline"
	case .unavailable(.clockUncertain): return "clock mismatch"
	}
}

private func percentText(_ value: Double?) -> String {
	guard let value else { return "—" }
	return String(format: "%.0f%%", value)
}

private func memoryPercentText(_ metrics: HMuxHostMetrics?) -> String {
	guard let used = metrics?.memoryUsedBytes, let total = metrics?.memoryTotalBytes, total > 0 else { return "—" }
	return percentText(Double(used) / Double(total) * 100)
}

private func memoryText(_ metrics: HMuxHostMetrics?) -> String {
	guard let metrics,
		let used = metrics.memoryUsedBytes,
		let total = metrics.memoryTotalBytes else { return "—" }
	return "\(memoryGigabytes(used))/\(memoryGigabytes(total)) GiB"
}

private func memoryDetail(_ metrics: HMuxHostMetrics) -> String {
	guard let used = metrics.memoryUsedBytes,
		let total = metrics.memoryTotalBytes,
		total > 0 else { return "Used and total memory are unavailable." }
	let percent = Double(used) / Double(total) * 100
	return "\(percentText(percent)) used of physical memory"
}

private func memoryGigabytes(_ bytes: UInt64) -> String {
	let gibibytes = Double(bytes) / 1_073_741_824
	if gibibytes >= 10 { return String(format: "%.0f", gibibytes) }
	if gibibytes >= 1 { return String(format: "%.1f", gibibytes) }
	return String(format: "%.2f", gibibytes)
}

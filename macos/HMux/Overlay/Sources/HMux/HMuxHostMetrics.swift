import Combine
import Foundation

enum HMuxHostMetricsUnavailableReason: Equatable {
	case noObservation
	case stale
	case offline
	case clockUncertain
}

enum HMuxHostMetricsAvailability: Equatable {
	case waiting
	case available
	case unavailable(HMuxHostMetricsUnavailableReason)
}

struct HMuxHostMetricsPresentation: Equatable {
	let metrics: HMuxHostMetrics?
	let availability: HMuxHostMetricsAvailability

	static let waiting = HMuxHostMetricsPresentation(metrics: nil, availability: .waiting)
}

struct HMuxHostMetricsObservation: Equatable {
	let metrics: HMuxHostMetrics
	let receivedAt: Date
}

enum HMuxHostMetricsPolicy {
	// The Home agent samples every five seconds. Four missed observations are
	// enough to stop presenting a value as current while tolerating brief jitter.
	static let maximumAge: TimeInterval = 20
	static let maximumFutureSkew: TimeInterval = 5
	static let maximumBackwardClockJump: TimeInterval = 5
	static let stalenessCheckNanoseconds: UInt64 = 1_000_000_000
}

func hmuxHostMetricsPresentation(
	observation: HMuxHostMetricsObservation?,
	hasReceivedCatalog: Bool,
	isOffline: Bool,
	now: Date
) -> HMuxHostMetricsPresentation {
	if isOffline {
		return HMuxHostMetricsPresentation(metrics: nil, availability: .unavailable(.offline))
	}
	guard let observation else {
		return hasReceivedCatalog
			? HMuxHostMetricsPresentation(metrics: nil, availability: .unavailable(.noObservation))
			: .waiting
	}

	let observationAge = now.timeIntervalSince(observation.metrics.observedDate)
	let receiptAge = now.timeIntervalSince(observation.receivedAt)
	if observationAge < -HMuxHostMetricsPolicy.maximumFutureSkew ||
		receiptAge < -HMuxHostMetricsPolicy.maximumBackwardClockJump {
		return HMuxHostMetricsPresentation(metrics: nil, availability: .unavailable(.clockUncertain))
	}
	if observationAge > HMuxHostMetricsPolicy.maximumAge ||
		receiptAge > HMuxHostMetricsPolicy.maximumAge {
		return HMuxHostMetricsPresentation(metrics: nil, availability: .unavailable(.stale))
	}
	return HMuxHostMetricsPresentation(metrics: observation.metrics, availability: .available)
}

@MainActor
final class HMuxHostMetricsStore: ObservableObject {
	@Published private(set) var presentation: HMuxHostMetricsPresentation = .waiting

	private var observation: HMuxHostMetricsObservation?
	private var hasReceivedCatalog = false
	private var isOffline = false
	private var stalenessTask: Task<Void, Never>?

	func start() {
		isOffline = false
		refreshAvailability()
		guard stalenessTask == nil else { return }
		stalenessTask = Task { [weak self] in
			while !Task.isCancelled {
				do {
					try await Task.sleep(nanoseconds: HMuxHostMetricsPolicy.stalenessCheckNanoseconds)
				} catch {
					return
				}
				guard let self else { return }
				self.refreshAvailability()
			}
		}
	}

	func stop() {
		stalenessTask?.cancel()
		stalenessTask = nil
		isOffline = true
		refreshAvailability()
	}

	func receive(_ metrics: HMuxHostMetrics?, receivedAt: Date = Date()) {
		hasReceivedCatalog = true
		isOffline = false
		guard let metrics else {
			observation = nil
			refreshAvailability(now: receivedAt)
			return
		}
		// Source, transport sequence and catalog ordering are checked by HMuxStore.
		// A corrected Home clock must not be fenced by an earlier future sample.
		observation = HMuxHostMetricsObservation(metrics: metrics, receivedAt: receivedAt)
		refreshAvailability(now: receivedAt)
	}

	func markOffline(now: Date = Date()) {
		isOffline = true
		refreshAvailability(now: now)
	}

	func refreshAvailability(now: Date = Date()) {
		let next = hmuxHostMetricsPresentation(
			observation: observation,
			hasReceivedCatalog: hasReceivedCatalog,
			isOffline: isOffline,
			now: now
		)
		if presentation != next { presentation = next }
	}
}

import Foundation

private enum HMuxHostMetricsSmokeError: Error {
	case failed(String)
}

@main
@MainActor
struct HMuxHostMetricsSmoke {
	static func main() throws {
		let valid = try decodeHostMetrics("""
		"host_metrics":{
			"observed_at":"2026-09-08T01:02:03.123456789Z",
			"cpu_percent":12.4,
			"gpu_percent":0,
			"memory_used_bytes":25769803776,
			"memory_total_bytes":68719476736
		}
		""")
		guard valid.observedAt == "2026-09-08T01:02:03.123456789Z",
			valid.cpuPercent == 12.4,
			valid.gpuPercent == 0,
			valid.memoryUsedBytes == 25_769_803_776,
			valid.memoryTotalBytes == 68_719_476_736 else {
			throw HMuxHostMetricsSmokeError.failed("valid host metrics")
		}

		let partial = try decodeHostMetrics("""
		"host_metrics":{"observed_at":"2026-09-08T01:02:04Z","cpu_percent":0}
		""")
		guard partial.cpuPercent == 0,
			partial.gpuPercent == nil,
			partial.memoryUsedBytes == nil,
			partial.memoryTotalBytes == nil else {
			throw HMuxHostMetricsSmokeError.failed("optional samples or real zero")
		}

		let legacy = try decodeCatalog("")
		guard legacy.hostMetrics == nil else {
			throw HMuxHostMetricsSmokeError.failed("legacy catalog")
		}

		for invalid in [
			"\"host_metrics\":{\"observed_at\":\"not-a-time\"}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\"}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"cpu_percent\":null,\"gpu_percent\":null}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03+09:00\"}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"cpu_percent\":-0.1}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"cpu_percent\":100.1}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"gpu_percent\":1e309}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"memory_used_bytes\":1}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"memory_total_bytes\":1}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"memory_used_bytes\":1,\"memory_total_bytes\":0}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"memory_used_bytes\":2,\"memory_total_bytes\":1}",
			"\"host_metrics\":{\"observed_at\":\"2026-09-08T01:02:03Z\",\"memory_used_bytes\":1,\"memory_total_bytes\":1152921504606846977}",
		] {
			try expectInvalid(invalid)
		}

		let observed = valid.observedDate
		let observation = HMuxHostMetricsObservation(metrics: valid, receivedAt: observed.addingTimeInterval(1))
		guard hmuxHostMetricsPresentation(
			observation: observation,
			hasReceivedCatalog: true,
			isOffline: false,
			now: observed.addingTimeInterval(5)
		) == HMuxHostMetricsPresentation(metrics: valid, availability: .available) else {
			throw HMuxHostMetricsSmokeError.failed("fresh presentation")
		}
		try expectUnavailable(.stale, presentation: hmuxHostMetricsPresentation(
			observation: observation,
			hasReceivedCatalog: true,
			isOffline: false,
			now: observed.addingTimeInterval(HMuxHostMetricsPolicy.maximumAge + 0.001)
		))
		try expectUnavailable(.clockUncertain, presentation: hmuxHostMetricsPresentation(
			observation: observation,
			hasReceivedCatalog: true,
			isOffline: false,
			now: observed.addingTimeInterval(-HMuxHostMetricsPolicy.maximumFutureSkew - 0.001)
		))
		try expectUnavailable(.offline, presentation: hmuxHostMetricsPresentation(
			observation: observation,
			hasReceivedCatalog: true,
			isOffline: true,
			now: observed
		))
		try expectUnavailable(.noObservation, presentation: hmuxHostMetricsPresentation(
			observation: nil,
			hasReceivedCatalog: true,
			isOffline: false,
			now: observed
		))
		guard hmuxHostMetricsPresentation(
			observation: nil,
			hasReceivedCatalog: false,
			isOffline: false,
			now: observed
		).availability == .waiting else {
			throw HMuxHostMetricsSmokeError.failed("initial waiting")
		}

		let store = HMuxHostMetricsStore()
		store.receive(valid, receivedAt: observed.addingTimeInterval(1))
		guard store.presentation.metrics == valid else {
			throw HMuxHostMetricsSmokeError.failed("store receives current observation")
		}
		let corrected = try decodeHostMetrics("""
		"host_metrics":{"observed_at":"2026-09-08T01:02:02Z","cpu_percent":99}
		""")
		store.receive(corrected, receivedAt: observed.addingTimeInterval(2))
		guard store.presentation.metrics == corrected else {
			throw HMuxHostMetricsSmokeError.failed("store accepts clock correction after catalog ordering")
		}
        let future = try decodeHostMetrics("\"host_metrics\":{\"observed_at\":\"2099-09-08T01:02:02Z\",\"cpu_percent\":99}")
        store.receive(future, receivedAt: observed.addingTimeInterval(2))
        try expectUnavailable(.clockUncertain, presentation: store.presentation)
        store.receive(valid, receivedAt: observed.addingTimeInterval(2))
        guard store.presentation.metrics == valid else {
            throw HMuxHostMetricsSmokeError.failed("future sample cannot lock recovery")
        }
		store.receive(nil, receivedAt: observed.addingTimeInterval(3))
		try expectUnavailable(.noObservation, presentation: store.presentation)
		store.receive(valid, receivedAt: observed.addingTimeInterval(4))
		store.markOffline(now: observed.addingTimeInterval(5))
		try expectUnavailable(.offline, presentation: store.presentation)

		print("host-metrics-ok optional-wire bounds zero stale offline clock-recovery fail-open")
	}

	private static func decodeHostMetrics(_ member: String) throws -> HMuxHostMetrics {
		guard let metrics = try decodeCatalog(member).hostMetrics else {
			throw HMuxHostMetricsSmokeError.failed("missing decoded host metrics")
		}
		return metrics
	}

	private static func decodeCatalog(_ member: String) throws -> HMuxCatalog {
		let separator = member.isEmpty ? "" : ","
		let json = """
		{"protocol_version":1,"generated_at":"2026-09-08T01:02:05Z","sessions":[]\(separator)\(member)}
		"""
		let decoder = JSONDecoder()
		decoder.keyDecodingStrategy = .convertFromSnakeCase
		return try decoder.decode(HMuxCatalog.self, from: Data(json.utf8))
	}

	private static func expectInvalid(_ member: String) throws {
        let catalog = try decodeCatalog(member)
        guard catalog.hostMetrics == nil, catalog.protocolVersion == 1, catalog.sessions.isEmpty else {
            throw HMuxHostMetricsSmokeError.failed("invalid metrics must preserve catalog")
        }
	}

	private static func expectUnavailable(
		_ reason: HMuxHostMetricsUnavailableReason,
		presentation: HMuxHostMetricsPresentation
	) throws {
		guard presentation.metrics == nil,
			presentation.availability == .unavailable(reason) else {
			throw HMuxHostMetricsSmokeError.failed("expected unavailable \(reason)")
		}
	}
}

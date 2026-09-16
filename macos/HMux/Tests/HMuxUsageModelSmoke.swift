import Foundation

@main
struct HMuxUsageModelSmoke {
    static func main() throws {
        let claude = try decode(provider: .claude, accounts: true)
        guard HMuxTokenUsageStore.remainingPercent(snapshot: claude) == 30,
			  claude.status.retryAt == nil,
              HMuxBedlBurnState(rawValueString: "rocket").cycleDuration == 0.5,
              HMuxBedlBurnState(rawValueString: "unknown") == .idle else {
            throw SmokeError.failed
        }

        let codex = try decode(provider: .codex, accounts: false)
        guard HMuxTokenUsageStore.remainingPercent(snapshot: codex) == 75 else {
            throw SmokeError.failed
        }
        // codex-lb pool weekly quota must remain independent of a depleted 5h
        // API-key allowance and of unequal per-account plan capacities.
        let poolFixture = fixture(provider: .codex, accounts: true)
            .replacingOccurrences(of: "\"state\":\"ok\"", with: "\"state\":\"ok\",\"quota_source\":\"codex_lb\"")
            .replacingOccurrences(of: "\"seq\":7", with: "\"seq\":7,\"weekly_observed\":true,\"rolling_5h_observed\":false")
            .replacingOccurrences(of: "\"number\":1", with: "\"number\":1,\"display_name\":\"Research account\"")
        let pool = try decodeJSON(poolFixture, provider: .codex)
        guard HMuxTokenUsageStore.remainingPercent(snapshot: pool) == 75,
              pool.accounts?.first?.label == "Research account",
              pool.accounts?[1].label == "Account 2",
              pool.quotaWindows[0].usedPct == nil,
              pool.quotaWindows[1].usedPct == 0.25 else { throw SmokeError.failed }
        let missingWeekly = try decodeJSON(poolFixture.replacingOccurrences(of: "\"weekly_observed\":true", with: "\"weekly_observed\":false"), provider: .codex)
        guard HMuxTokenUsageStore.remainingPercent(snapshot: missingWeekly) == nil,
              !HMuxProviderUsageState(phase: .connected, snapshot: missingWeekly).hasUsableQuota else { throw SmokeError.failed }
        let fullWeekly = try decodeJSON(poolFixture.replacingOccurrences(of: "\"used_pct\":0.25", with: "\"used_pct\":0.0"), provider: .codex)
        guard HMuxTokenUsageStore.remainingPercent(snapshot: fullWeekly) == 100 else { throw SmokeError.failed }
        let noClaudeAccountQuota = fixture(provider: .claude, accounts: true)
            .replacingOccurrences(of: "\"status\":\"ok\"", with: "\"status\":\"unavailable\"")
        guard HMuxTokenUsageStore.remainingPercent(snapshot: try decodeJSON(noClaudeAccountQuota, provider: .claude)) == nil else { throw SmokeError.failed }
        let missingClaudeWeekly = try decodeJSON(
            fixture(provider: .claude, accounts: false)
                .replacingOccurrences(of: "\"seq\":7", with: "\"seq\":7,\"weekly_observed\":false"),
            provider: .claude
        )
        guard HMuxTokenUsageStore.remainingPercent(snapshot: missingClaudeWeekly) == nil,
              !HMuxProviderUsageState(phase: .connected, snapshot: missingClaudeWeekly).hasUsableQuota else {
            throw SmokeError.failed
        }
		let limitedFixture = fixture(provider: .claude, accounts: false)
			.replacingOccurrences(
				of: "\"state\":\"ok\"",
				with: "\"state\":\"rateLimited\",\"retry_at\":\"2026-08-23T12:38:27.000Z\""
			)
		let limitedClaude = try decodeJSON(limitedFixture, provider: .claude)
		let limitedState = HMuxProviderUsageState(phase: .connected, snapshot: limitedClaude)
		guard !limitedState.hasUsableQuota,
			  limitedClaude.status.retryAt == "2026-08-23T12:38:27.000Z",
			  limitedClaude.status.retryDate != nil,
              HMuxTokenUsageStore.summaryStatus(state: limitedState, hasQuota: false) == .limited else {
            throw SmokeError.failed
        }
		let staleRetry = try decodeJSON(
			fixture(provider: .claude, accounts: false)
				.replacingOccurrences(
					of: "\"state\":\"ok\"",
					with: "\"state\":\"ok\",\"retry_at\":\"2026-08-23T12:38:27.000Z\""
				)
				.replacingOccurrences(of: "\"stale\":false", with: "\"stale\":true"),
			provider: .claude
		)
		guard staleRetry.status.retryDate != nil else { throw SmokeError.failed }
		for invalidRetry in [
			"soon",
			"2026-08-23T11:59:59.000Z",
			"2026-08-23T12:00:00.000Z",
			"2026-08-24T12:00:00.001Z",
			String(repeating: "2", count: 129),
		] {
			let encoded = String(data: try JSONEncoder().encode(invalidRetry), encoding: .utf8)!
			try expectInvalid(
				limitedFixture.replacingOccurrences(
					of: "\"2026-08-23T12:38:27.000Z\"",
					with: encoded
				),
				provider: .claude
			)
		}
		try expectInvalid(
			limitedFixture.replacingOccurrences(of: "\"state\":\"rateLimited\"", with: "\"state\":\"networkError\""),
			provider: .claude
		)
        for invalidName in [String(repeating: "x", count: 257), "unsafe\nname", "unsafe\u{202E}name"] {
            let encoded = String(data: try JSONEncoder().encode(invalidName), encoding: .utf8)!
            try expectInvalid(poolFixture.replacingOccurrences(of: "\"Research account\"", with: encoded), provider: .codex)
        }
        try expectInvalid(poolFixture.replacingOccurrences(of: "\"number\":2", with: "\"number\":1"), provider: .codex)
        try expectInvalid(poolFixture.replacingOccurrences(of: "\"number\":1", with: "\"number\":0"), provider: .codex)
		var degradedState = HMuxProviderUsageState(phase: .connected, snapshot: codex)
		guard degradedState.hasUsableQuota else { throw SmokeError.failed }
		let degradedJSON = fixture(provider: .codex, accounts: false)
			.replacingOccurrences(of: "\"state\":\"ok\"", with: "\"state\":\"networkError\"")
		degradedState.snapshot = try JSONDecoder().decode(
			HMuxUsageSnapshot.self,
			from: Data(degradedJSON.utf8)
		).validated(for: .codex)
		guard !degradedState.hasUsableQuota else { throw SmokeError.failed }
		guard HMuxTokenUsageStore.summaryStatus(state: degradedState, hasQuota: false) == .network,
			HMuxTokenUsageStore.summaryStatus(state: HMuxProviderUsageState(), hasQuota: false) == .starting,
			HMuxTokenUsageStore.summaryStatus(
				state: HMuxProviderUsageState(phase: .offline),
				hasQuota: false
			) == .offline
		else { throw SmokeError.failed }
		var offlineState = HMuxProviderUsageState(phase: .offline, snapshot: codex)
		guard offlineState.isStale else { throw SmokeError.failed }
		offlineState.phase = .connected
		guard !offlineState.isStale else { throw SmokeError.failed }
		let emptyOfflineState = HMuxProviderUsageState(phase: .offline)
		guard !emptyOfflineState.isStale else { throw SmokeError.failed }
		var restartBackoff = HMuxUsageRestartBackoff()
		guard (0..<8).map({ _ in restartBackoff.consumeDelay() }) == [1, 2, 4, 8, 16, 32, 60, 60] else {
			throw SmokeError.failed
		}
		restartBackoff.markStable()
		guard restartBackoff.consumeDelay() == 1 else { throw SmokeError.failed }

		let snapshotFrameJSON = """
		{"protocol_version":1,"sequence":1,"type":"snapshot","provider":"codex","snapshot":\(fixture(provider: .codex, accounts: false))}
		"""
		let decoder = JSONDecoder()
		let snapshotFrame = try decoder.decode(
			HMuxUsageStreamFrame.self,
			from: Data(snapshotFrameJSON.utf8)
		)
		guard case .snapshot(let validatedSnapshot) = try snapshotFrame.validated(expectedSequence: 1),
			validatedSnapshot.provider == .codex else {
			throw SmokeError.failed
		}
		let heartbeatFrame = try decoder.decode(
			HMuxUsageStreamFrame.self,
			from: Data("{\"protocol_version\":1,\"sequence\":2,\"type\":\"heartbeat\"}".utf8)
		)
		guard case .heartbeat = try heartbeatFrame.validated(expectedSequence: 2) else { throw SmokeError.failed }
		let updateFrame = try decoder.decode(
			HMuxUsageStreamFrame.self,
			from: Data("{\"protocol_version\":1,\"sequence\":3,\"type\":\"status\",\"code\":\"home_agent_update_required\"}".utf8)
		)
		guard case .homeAgentUpdateRequired = try updateFrame.validated(expectedSequence: 3) else { throw SmokeError.failed }
		guard HMuxTokenUsageStore.summaryStatus(
			state: HMuxProviderUsageState(phase: .updateRequired),
			hasQuota: false
		) == .updateRequired else { throw SmokeError.failed }
		let updateWithSnapshot = HMuxProviderUsageState(
			phase: .updateRequired,
			snapshot: codex,
			transportStale: true
		)
		guard !updateWithSnapshot.hasUsableQuota,
			HMuxTokenUsageStore.summaryStatus(
				state: updateWithSnapshot,
				hasQuota: updateWithSnapshot.hasUsableQuota
			) == .updateRequired
		else { throw SmokeError.failed }
		var parser = HMuxUsageNDJSONParser()
		var parsedLine: Data?
		for byte in Data((snapshotFrameJSON + "\n").utf8) {
			if let line = try parser.consume(byte) { parsedLine = line }
		}
		guard parsedLine == Data(snapshotFrameJSON.utf8) else { throw SmokeError.failed }

        let invalid = fixture(provider: .codex, accounts: false)
            .replacingOccurrences(of: "\"used_pct\":0.5", with: "\"used_pct\":1.2")
        do {
            _ = try JSONDecoder().decode(HMuxUsageSnapshot.self, from: Data(invalid.utf8))
                .validated(for: .codex)
            throw SmokeError.failed
        } catch SmokeError.failed {
            throw SmokeError.failed
        } catch {
            // Expected validation failure.
        }

        let cswapEmail = fixture(provider: .claude, accounts: true)
            .replacingOccurrences(of: "\"email\":\"\"", with: "\"email\":\"owner@example.test\"")
        guard try decodeJSON(cswapEmail, provider: .claude).accounts?.first?.label == "owner@example.test" else { throw SmokeError.failed }
        let switched = cswapEmail
            .replacingOccurrences(of: "\"active\":true", with: "\"active\":false")
            .replacingOccurrences(of: "\"number\":2,\"email\":\"owner@example.test\",\"active\":false", with: "\"number\":2,\"email\":\"owner@example.test\",\"active\":true")
        guard HMuxTokenUsageStore.remainingPercent(snapshot: try decodeJSON(switched, provider: .claude)) == 10 else { throw SmokeError.failed }
        let ambiguous = cswapEmail.replacingOccurrences(of: "\"active\":false", with: "\"active\":true")
        guard HMuxTokenUsageStore.remainingPercent(snapshot: try decodeJSON(ambiguous, provider: .claude)) == nil else { throw SmokeError.failed }
        try expectInvalid(fixture(provider: .codex, accounts: true).replacingOccurrences(of: "\"email\":\"\"", with: "\"email\":\"private@example.test\""), provider: .codex)
        print("usage-models-ok weekly-pool account-names missing-windows retry-at validation")
    }

    private static func decodeJSON(_ json: String, provider: HMuxUsageProvider) throws -> HMuxUsageSnapshot {
        try JSONDecoder().decode(HMuxUsageSnapshot.self, from: Data(json.utf8)).validated(for: provider)
    }

    private static func expectInvalid(_ json: String, provider: HMuxUsageProvider) throws {
        do {
            _ = try decodeJSON(json, provider: provider)
            throw SmokeError.failed
        } catch SmokeError.failed {
            throw SmokeError.failed
        } catch {}
    }

    private static func decode(provider: HMuxUsageProvider, accounts: Bool) throws -> HMuxUsageSnapshot {
        try JSONDecoder().decode(
            HMuxUsageSnapshot.self,
            from: Data(fixture(provider: provider, accounts: accounts).utf8)
        ).validated(for: provider)
    }

    private static func fixture(provider: HMuxUsageProvider, accounts: Bool) -> String {
        let accountJSON = accounts ? """
        ,"accounts":[
          {"number":1,"email":"","active":true,"status":"ok","five_hour":{"used_pct":0.2,"resets_at":null},"seven_day":{"used_pct":0.7,"resets_at":null}},
          {"number":2,"email":"","active":false,"status":"ok","five_hour":{"used_pct":0.4,"resets_at":null},"seven_day":{"used_pct":0.9,"resets_at":null}},
          {"number":3,"email":"","active":false,"status":"paused","five_hour":{"used_pct":1.0,"resets_at":null},"seven_day":{"used_pct":1.0,"resets_at":null}}
        ]
        """ : ""
        return """
        {"schema":1,"seq":7,"generated_at_utc":"2026-08-23T12:00:00Z","provider":"\(provider.rawValue)","burn_rate_per_min":10,"burn_state":"run","today_total_tokens":12000,"today_sessions":2,"rolling_5h":{"used_pct":0.5,"remaining_seconds":60,"resets_at":null},"weekly":{"used_pct":0.25,"remaining_seconds":120,"resets_at":null},"status":{"state":"ok","data_source":"api_only","stale":false,"quota_observed_at":null}\(accountJSON)}
        """
    }

    private enum SmokeError: Error { case failed }
}

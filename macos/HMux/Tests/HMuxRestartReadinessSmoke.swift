import Darwin
import Foundation

private enum HMuxRestartReadinessSmokeError: Error {
	case invalidEnvironment
	case missingSignal
	case acceptedTampering
	case environmentNotCleared
}

@main
struct HMuxRestartReadinessSmoke {
	static func main() throws {
		let request = try HMuxRestartReadiness.makeRequest()
		defer { HMuxRestartReadiness.cleanup(request) }
		let environment = [
			HMuxRestartReadiness.directoryEnvironmentKey: request.directoryURL.path,
			HMuxRestartReadiness.nonceEnvironmentKey: request.nonce,
		]
		guard HMuxRestartReadiness.request(from: environment) == request,
		      request.launchArguments.count == 4,
		      HMuxRestartReadiness.readyPID(for: request) == nil else {
			throw HMuxRestartReadinessSmokeError.invalidEnvironment
		}

		setenv(HMuxRestartReadiness.directoryEnvironmentKey, request.directoryURL.path, 1)
		setenv(HMuxRestartReadiness.nonceEnvironmentKey, request.nonce, 1)
		HMuxRestartReadiness.signalIfRequested()
		guard getenv(HMuxRestartReadiness.directoryEnvironmentKey) == nil,
		      getenv(HMuxRestartReadiness.nonceEnvironmentKey) == nil else {
			throw HMuxRestartReadinessSmokeError.environmentNotCleared
		}
		guard HMuxRestartReadiness.readyPID(for: request) == getpid() else {
			throw HMuxRestartReadinessSmokeError.missingSignal
		}

		var tamperedEnvironment = environment
		tamperedEnvironment[HMuxRestartReadiness.nonceEnvironmentKey] = UUID().uuidString.lowercased()
		guard HMuxRestartReadiness.request(from: tamperedEnvironment) == nil else {
			throw HMuxRestartReadinessSmokeError.acceptedTampering
		}

		let symlinkRequest = try HMuxRestartReadiness.makeRequest()
		defer { HMuxRestartReadiness.cleanup(symlinkRequest) }
		guard Darwin.symlink("/dev/null", symlinkRequest.signalURL.path) == 0 else {
			throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
		}
		guard HMuxRestartReadiness.readyPID(for: symlinkRequest) == nil else {
			throw HMuxRestartReadinessSmokeError.acceptedTampering
		}

		let modeRequest = try HMuxRestartReadiness.makeRequest()
		defer { HMuxRestartReadiness.cleanup(modeRequest) }
		try HMuxRestartReadiness.writeSignal(for: modeRequest, pid: getpid())
		guard Darwin.chmod(modeRequest.signalURL.path, mode_t(0o644)) == 0 else {
			throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
		}
		guard HMuxRestartReadiness.readyPID(for: modeRequest) == nil else {
			throw HMuxRestartReadinessSmokeError.acceptedTampering
		}

		let hardlinkRequest = try HMuxRestartReadiness.makeRequest()
		defer { HMuxRestartReadiness.cleanup(hardlinkRequest) }
		try HMuxRestartReadiness.writeSignal(for: hardlinkRequest, pid: getpid())
		let secondLink = hardlinkRequest.directoryURL.appendingPathComponent("second-link").path
		guard Darwin.link(hardlinkRequest.signalURL.path, secondLink) == 0 else {
			throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
		}
		defer { _ = Darwin.unlink(secondLink) }
		guard HMuxRestartReadiness.readyPID(for: hardlinkRequest) == nil else {
			throw HMuxRestartReadinessSmokeError.acceptedTampering
		}

		print("restart-readiness-ok")
	}
}

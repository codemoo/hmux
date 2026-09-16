import Darwin
import Foundation

private enum HMuxBackendLifecycleSmokeError: Error {
	case helperDidNotStart
	case helperSurvivedCancellation
	case unexpectedSuccess
}

@main
struct HMuxBackendLifecycleSmoke {
	static func main() async throws {
		guard CommandLine.arguments.count == 3 else {
			throw HMuxBackendError.commandFailed
		}
		let helper = CommandLine.arguments[1]
		let pidFile = CommandLine.arguments[2]
		setenv("HMUX_EXECUTABLE", helper, 1)
		setenv("HMUX_TEST_PID_FILE", pidFile, 1)

		let operation = Task { try await HMuxBackend.loadCatalog() }
		let startDeadline = Date().addingTimeInterval(3)
		var pid: pid_t = 0
		while Date() < startDeadline {
			if let text = try? String(contentsOfFile: pidFile, encoding: .utf8),
			   let value = Int32(text.trimmingCharacters(in: .whitespacesAndNewlines)),
			   value > 0 {
				pid = value
				break
			}
			try await Task.sleep(nanoseconds: 20_000_000)
		}
		guard pid > 0 else {
			operation.cancel()
			throw HMuxBackendLifecycleSmokeError.helperDidNotStart
		}
		operation.cancel()
		do {
			_ = try await operation.value
			throw HMuxBackendLifecycleSmokeError.unexpectedSuccess
		} catch is CancellationError {
			// Expected.
		} catch {
			// Closing a pipe during cancellation may surface a Foundation I/O
			// error; process teardown is the authoritative assertion below.
		}

		let exitDeadline = Date().addingTimeInterval(4)
		while Date() < exitDeadline {
			if Darwin.kill(pid, 0) == -1, errno == ESRCH {
				print("backend-lifecycle-ok")
				return
			}
			try await Task.sleep(nanoseconds: 20_000_000)
		}
		throw HMuxBackendLifecycleSmokeError.helperSurvivedCancellation
	}
}

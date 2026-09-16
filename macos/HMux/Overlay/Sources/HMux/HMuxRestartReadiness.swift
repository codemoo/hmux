import Darwin
import Foundation

struct HMuxRestartReadinessRequest: Equatable {
	let directoryURL: URL
	let signalURL: URL
	let nonce: String

	var launchArguments: [String] {
		[
			"--env", "\(HMuxRestartReadiness.directoryEnvironmentKey)=\(directoryURL.path)",
			"--env", "\(HMuxRestartReadiness.nonceEnvironmentKey)=\(nonce)",
		]
	}
}

enum HMuxRestartReadiness {
	static let directoryEnvironmentKey = "HMUX_RESTART_READY_DIRECTORY"
	static let nonceEnvironmentKey = "HMUX_RESTART_READY_NONCE"

	private static let directoryPrefix = "hmux-restart-ready."
	private static let signalName = "ready"
	private static let maximumSignalBytes: off_t = 128

	static func makeRequest(
		temporaryDirectory: URL = FileManager.default.temporaryDirectory
	) throws -> HMuxRestartReadinessRequest {
		let baseURL = temporaryDirectory.standardizedFileURL
		guard baseURL.isFileURL, baseURL.path.hasPrefix("/") else {
			throw CocoaError(.fileWriteInvalidFileName)
		}

		for _ in 0..<8 {
			let nonce = UUID().uuidString.lowercased()
			let directoryURL = baseURL.appendingPathComponent(directoryPrefix + nonce, isDirectory: true)
			if Darwin.mkdir(directoryURL.path, mode_t(S_IRWXU)) == 0 {
				do {
					try validateOwnedPrivateDirectory(directoryURL)
					return HMuxRestartReadinessRequest(
						directoryURL: directoryURL,
						signalURL: directoryURL.appendingPathComponent(signalName, isDirectory: false),
						nonce: nonce
					)
				} catch {
					_ = Darwin.rmdir(directoryURL.path)
					throw error
				}
			}
			guard errno == EEXIST else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
		}
		throw POSIXError(.EEXIST)
	}

	static func request(
		from environment: [String: String] = ProcessInfo.processInfo.environment,
		temporaryDirectory: URL = FileManager.default.temporaryDirectory
	) -> HMuxRestartReadinessRequest? {
		guard let rawDirectory = environment[directoryEnvironmentKey],
		      let nonce = environment[nonceEnvironmentKey],
		      validNonce(nonce),
		      !rawDirectory.utf8.contains(0),
		      rawDirectory.hasPrefix("/") else { return nil }

		let baseURL = temporaryDirectory.standardizedFileURL
		let directoryURL = URL(fileURLWithPath: rawDirectory, isDirectory: true).standardizedFileURL
		guard directoryURL.path == rawDirectory,
		      directoryURL.deletingLastPathComponent().standardizedFileURL == baseURL,
		      directoryURL.lastPathComponent == directoryPrefix + nonce,
		      (try? validateOwnedPrivateDirectory(directoryURL)) != nil else { return nil }

		return HMuxRestartReadinessRequest(
			directoryURL: directoryURL,
			signalURL: directoryURL.appendingPathComponent(signalName, isDirectory: false),
			nonce: nonce
		)
	}

	static func signalIfRequested() {
		defer {
			unsetenv(directoryEnvironmentKey)
			unsetenv(nonceEnvironmentKey)
		}
		guard let request = request() else { return }
		try? writeSignal(for: request, pid: getpid())
	}

	static func writeSignal(for request: HMuxRestartReadinessRequest, pid: pid_t) throws {
		guard pid > 0, validNonce(request.nonce) else { throw POSIXError(.EINVAL) }
		try validateOwnedPrivateDirectory(request.directoryURL)
		let expectedSignalURL = request.directoryURL.appendingPathComponent(signalName, isDirectory: false)
		guard request.signalURL.standardizedFileURL == expectedSignalURL.standardizedFileURL else {
			throw POSIXError(.EINVAL)
		}

		let descriptor = Darwin.open(
			request.signalURL.path,
			O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
			mode_t(S_IRUSR | S_IWUSR)
		)
		guard descriptor >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
		defer { _ = Darwin.close(descriptor) }

		let payload = Array("\(request.nonce)\n\(pid)\n".utf8)
		guard payload.count <= Int(maximumSignalBytes) else { throw POSIXError(.EFBIG) }
		try payload.withUnsafeBytes { bytes in
			guard let baseAddress = bytes.baseAddress else { throw POSIXError(.EIO) }
			var written = 0
			while written < bytes.count {
				let count = Darwin.write(descriptor, baseAddress.advanced(by: written), bytes.count - written)
				if count > 0 {
					written += count
					continue
				}
				if count == -1, errno == EINTR { continue }
				throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
			}
		}
		guard Darwin.fsync(descriptor) == 0 else {
			throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
		}
	}

	static func readyPID(for request: HMuxRestartReadinessRequest) -> pid_t? {
		guard validNonce(request.nonce),
		      (try? validateOwnedPrivateDirectory(request.directoryURL)) != nil else { return nil }

		var before = stat()
		guard lstat(request.signalURL.path, &before) == 0,
		      (before.st_mode & S_IFMT) == S_IFREG,
		      before.st_uid == geteuid(),
		      before.st_nlink == 1,
		      (before.st_mode & mode_t(0o777)) == mode_t(0o600),
		      before.st_size > 0,
		      before.st_size <= maximumSignalBytes else { return nil }

		let descriptor = Darwin.open(request.signalURL.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
		guard descriptor >= 0 else { return nil }
		defer { _ = Darwin.close(descriptor) }
		var after = stat()
		guard fstat(descriptor, &after) == 0,
		      after.st_dev == before.st_dev,
		      after.st_ino == before.st_ino,
		      after.st_uid == before.st_uid,
		      after.st_nlink == before.st_nlink,
		      after.st_size == before.st_size else { return nil }

		var payload = [UInt8](repeating: 0, count: Int(after.st_size))
		var offset = 0
		while offset < payload.count {
			let remaining = payload.count - offset
			let count = payload.withUnsafeMutableBytes { bytes in
				Darwin.read(descriptor, bytes.baseAddress!.advanced(by: offset), remaining)
			}
			if count > 0 {
				offset += count
				continue
			}
			if count == -1, errno == EINTR { continue }
			return nil
		}
		guard let text = String(bytes: payload, encoding: .utf8) else { return nil }
		let fields = text.split(separator: "\n", omittingEmptySubsequences: true)
		guard fields.count == 2,
		      fields[0] == Substring(request.nonce),
		      let parsedPID = Int32(fields[1]),
		      parsedPID > 0,
		      text == "\(request.nonce)\n\(parsedPID)\n" else { return nil }
		return parsedPID
	}

	static func cleanup(_ request: HMuxRestartReadinessRequest) {
		_ = Darwin.unlink(request.signalURL.path)
		_ = Darwin.rmdir(request.directoryURL.path)
	}

	private static func validateOwnedPrivateDirectory(_ url: URL) throws {
		var info = stat()
		guard lstat(url.path, &info) == 0 else {
			throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
		}
		guard (info.st_mode & S_IFMT) == S_IFDIR,
		      info.st_uid == geteuid(),
		      (info.st_mode & mode_t(0o077)) == 0 else { throw POSIXError(.EPERM) }
	}

	private static func validNonce(_ nonce: String) -> Bool {
		guard nonce.utf8.count == 36, UUID(uuidString: nonce) != nil else { return false }
		return nonce.utf8.allSatisfy { byte in
			(byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102) || byte == 45
		}
	}
}

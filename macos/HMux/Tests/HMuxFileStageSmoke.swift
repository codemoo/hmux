import Foundation
import Darwin

@main
struct HMuxFileStageSmoke {
	static func main() async throws {
		let fileManager = FileManager.default
		let directory = fileManager.temporaryDirectory
			.appendingPathComponent("hmux-file-stage-smoke-\(UUID().uuidString)", isDirectory: true)
		try fileManager.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
		defer { try? fileManager.removeItem(at: directory) }

		let executable = directory.appendingPathComponent("hmux-fake")
		let argumentsFile = directory.appendingPathComponent("args")
		let parentFile = directory.appendingPathComponent("parent")
		let childPIDFile = directory.appendingPathComponent("child-pid")
		let responseFile = directory.appendingPathComponent("response")
		let sourceFile = directory.appendingPathComponent("local secret name.png")
		try Data("opaque".utf8).write(to: sourceFile, options: .atomic)
		let script = """
		#!/bin/sh
		set -eu
		printf '%s\\n' "$@" > "$HMUX_FAKE_ARGS"
		printf '%s\\n' "$HMUX_FILE_STAGE_PARENT_PID" > "$HMUX_FAKE_PARENT"
		printf '%s\\n' "$$" > "$HMUX_FAKE_CHILD_PID"
		if [ "${HMUX_FAKE_MODE:-ok}" = no-read ]; then trap '' TERM; exec sleep 60; fi
		cat >/dev/null
		if [ "${HMUX_FAKE_MODE:-ok}" = hang ]; then exec sleep 60; fi
		cat "$HMUX_FAKE_RESPONSE"
		"""
		try Data(script.utf8).write(to: executable)
		try fileManager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: executable.path)

		let requestID = "00112233445566778899aabbccddeeff"
		let stageID = "ffeeddccbbaa99887766554433221100"
		let response = """
		{"app_protocol_version":1,"ok":true,"data":{"protocol_version":1,"request_id":"\(requestID)","stage_id":"\(stageID)","session":{"id":"$7","created_at":1700000000},"expires_at_unix":1700086400,"files":[{"index":0,"path":"/Users/home/Library/Caches/hmux/staged-files-v1/1700086400-\(stageID)/file-0001.png","size":6,"sha256":"\(String(repeating: "a", count: 64))"}]}}
		"""
		try Data(response.utf8).write(to: responseFile)
		setenv("HMUX_EXECUTABLE", executable.path, 1)
		setenv("HMUX_FAKE_ARGS", argumentsFile.path, 1)
		setenv("HMUX_FAKE_PARENT", parentFile.path, 1)
		setenv("HMUX_FAKE_CHILD_PID", childPIDFile.path, 1)
		setenv("HMUX_FAKE_RESPONSE", responseFile.path, 1)
		setenv("HMUX_FAKE_MODE", "ok", 1)

		let session = HMuxSessionIdentity(id: "$7", createdAt: 1_700_000_000)
		let result = try await HMuxBackend.stageFiles([sourceFile], for: session, requestID: requestID)
		guard result.requestID == requestID, result.files.count == 1 else {
			throw HMuxBackendError.invalidFileStage
		}
		let arguments = try String(contentsOf: argumentsFile, encoding: .utf8)
		guard arguments == "--no-update-check\napp\nfile-stage\n" else {
			throw HMuxBackendError.commandFailed
		}
		let parent = try String(contentsOf: parentFile, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
		guard parent == String(ProcessInfo.processInfo.processIdentifier) else {
			throw HMuxBackendError.commandFailed
		}

		try? fileManager.removeItem(at: childPIDFile)
		setenv("HMUX_FAKE_MODE", "no-read", 1)
		let longURLs = (0..<15).map { index in
			URL(fileURLWithPath: "/\(String(repeating: "a", count: 3_800))-\(index).png")
		}
		let started = Date()
		let task = Task {
			try await HMuxBackend.stageFiles(longURLs, for: session, requestID: requestID)
		}
		let childPID = try await waitForChildPID(at: childPIDFile)
		task.cancel()
		do {
			_ = try await task.value
			throw HMuxBackendError.commandFailed
		} catch {
			guard task.isCancelled else { throw error }
		}
		guard Date().timeIntervalSince(started) < 3 else {
			throw HMuxBackendError.commandFailed
		}
		errno = 0
		guard Darwin.kill(childPID, 0) == -1, errno == ESRCH else {
			throw HMuxBackendError.commandFailed
		}
		print("file-stage-ok")
	}

	private static func waitForChildPID(at url: URL) async throws -> pid_t {
		for _ in 0..<100 {
			if let value = try? String(contentsOf: url, encoding: .utf8),
			   let pid = pid_t(value.trimmingCharacters(in: .whitespacesAndNewlines)), pid > 0 {
				return pid
			}
			try await Task.sleep(nanoseconds: 20_000_000)
		}
		throw HMuxBackendError.commandFailed
	}
}

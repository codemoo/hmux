import Foundation

struct HMuxBackend {
	static func loadConversation(session: HMuxSessionIdentity) async throws -> HMuxConversation {
		try await Task.sleep(nanoseconds: 10_000_000)
		return try HMuxConversation(
			sessionID: session.id,
			createdAt: session.createdAt,
			status: .ready,
			messages: [],
			truncated: false
		)
	}
}

@main
struct HMuxConversationSmoke {
	static func main() async throws {
		let response = """
		{"app_protocol_version":1,"ok":true,"data":{"session_id":"$42","created_at":1700000000,"status":"ready","messages":[{"id":"q1","role":"user","text":"How do I ship this?"},{"id":"a1","role":"assistant","text":"Use **one** command.\\n```sh\\necho secret\\n```\\nThen verify café output."}],"truncated":false},"error":null}
		"""
		let conversation = try HMuxConversation.decodeEnvelope(Data(response.utf8))
		guard conversation.sessionID == "$42",
		      conversation.createdAt == 1_700_000_000,
		      conversation.status == .ready,
		      conversation.messages.count == 2,
		      !conversation.truncated else {
			throw SmokeError.failed("valid envelope")
		}

		let answers = hmuxConversationDisplayMessages(
			conversation,
			showQuestions: false,
			showCode: false,
			query: "CAFE verify"
		)
		guard answers.count == 1,
		      answers[0].role == .assistant,
		      answers[0].text == "Use **one** command.\nThen verify café output.",
		      !answers[0].text.contains("secret") else {
			throw SmokeError.failed("answer filtering and fenced code removal")
		}

		let allMessages = hmuxConversationDisplayMessages(
			conversation,
			showQuestions: true,
			showCode: true,
			query: ""
		)
		guard allMessages.count == 2,
		      allMessages[0].role == .user,
		      allMessages[1].text.contains("echo secret") else {
			throw SmokeError.failed("question and code options")
		}

		let tildeFence = "before\n  ~~~~swift\nlet value = 1\n  ~~~~   \nafter"
		guard hmuxConversationRemovingFencedCode(from: tildeFence) == "before\nafter" else {
			throw SmokeError.failed("tilde fence")
		}
		let indentedFence = "before\n    ```\nkept\n    ```\nafter"
		guard hmuxConversationRemovingFencedCode(from: indentedFence) == indentedFence else {
			throw SmokeError.failed("four-space indentation")
		}

		try expectDecodeFailure(response.replacingOccurrences(of: "\"role\":\"user\"", with: "\"role\":\"tool\""))
		try expectDecodeFailure(response.replacingOccurrences(of: "\"status\":\"ready\"", with: "\"status\":\"unknown\""))
		try expectDecodeFailure(response.replacingOccurrences(of: "\"app_protocol_version\":1", with: "\"app_protocol_version\":2"))

		let messages = (0...HMuxConversation.maximumMessages).map { index in
			"{\"id\":\"m\(index)\",\"role\":\"assistant\",\"text\":\"x\"}"
		}.joined(separator: ",")
		try expectDecodeFailure(envelope(messages: messages))

		let oversizedText = String(repeating: "x", count: HMuxConversation.maximumTextBytes + 1)
		let oversizedMessage = "{\"id\":\"large\",\"role\":\"assistant\",\"text\":\"\(oversizedText)\"}"
		try expectDecodeFailure(envelope(messages: oversizedMessage))

		let cancellable = Task {
			try await HMuxBackend.loadConversation(
				session: HMuxSessionIdentity(id: "$42", createdAt: 1_700_000_000)
			)
		}
		cancellable.cancel()
		do {
			_ = try await cancellable.value
			throw SmokeError.failed("backend cancellation")
		} catch is CancellationError {
			// A disappearing or replaced SwiftUI task cancels the awaited backend operation.
		}

		print("conversation-ok decode-bounds filtering code-stripping cancellation view-typecheck")
	}

	private static func envelope(messages: String) -> String {
		"""
		{"app_protocol_version":1,"ok":true,"data":{"session_id":"$42","created_at":1700000000,"status":"ready","messages":[\(messages)],"truncated":false},"error":null}
		"""
	}

	private static func expectDecodeFailure(_ json: String) throws {
		do {
			_ = try HMuxConversation.decodeEnvelope(Data(json.utf8))
			throw SmokeError.failed("expected decode failure")
		} catch SmokeError.failed(let reason) {
			throw SmokeError.failed(reason)
		} catch {
			// Expected.
		}
	}

	private enum SmokeError: Error {
		case failed(String)
	}
}

import Foundation

enum HMuxConversationDecodeError: Error, Equatable {
	case invalidEnvelope
	case invalidConversation
	case tooManyMessages
	case textLimitExceeded
}

struct HMuxConversation: Decodable, Equatable, Sendable {
	enum Status: String, Decodable, Sendable {
		case ready
		case unavailable
		case ambiguous
	}

	enum Role: String, Decodable, Sendable {
		case assistant
		case user
	}

	struct Message: Decodable, Equatable, Identifiable, Sendable {
		let id: String
		let role: Role
		let text: String
	}

	static let maximumMessages = 200
	static let maximumTextBytes = 512 * 1024
	static let maximumMessageTextBytes = 256 * 1024
	static let maximumEncodedResponseBytes = 2 * 1024 * 1024

	let sessionID: String
	let createdAt: Int64
	let status: Status
	let messages: [Message]
	let truncated: Bool

	private enum CodingKeys: String, CodingKey {
		case sessionID = "sessionId"
		case createdAt
		case status
		case messages
		case truncated
	}

	init(
		sessionID: String,
		createdAt: Int64,
		status: Status,
		messages: [Message],
		truncated: Bool
	) throws {
		try Self.validate(
			sessionID: sessionID,
			createdAt: createdAt,
			messages: messages,
			maximumMessages: Self.maximumMessages,
			maximumTextBytes: Self.maximumTextBytes
		)
        guard status == .ready || messages.isEmpty else {
            throw HMuxConversationDecodeError.invalidConversation
        }
        self.sessionID = sessionID
        self.createdAt = createdAt
		self.status = status
		self.messages = messages
		self.truncated = truncated
	}

	init(from decoder: Decoder) throws {
		let container = try decoder.container(keyedBy: CodingKeys.self)
		let sessionID = try container.decode(String.self, forKey: .sessionID)
		let createdAt = try container.decode(Int64.self, forKey: .createdAt)
		let status = try container.decode(Status.self, forKey: .status)
		let messages = try container.decode([Message].self, forKey: .messages)
		let truncated = try container.decode(Bool.self, forKey: .truncated)
		try Self.validate(
			sessionID: sessionID,
			createdAt: createdAt,
			messages: messages,
			maximumMessages: Self.maximumMessages,
			maximumTextBytes: Self.maximumTextBytes
		)
        guard status == .ready || messages.isEmpty else {
            throw HMuxConversationDecodeError.invalidConversation
        }
        self.sessionID = sessionID
        self.createdAt = createdAt
		self.status = status
		self.messages = messages
		self.truncated = truncated
	}

	static func decodeEnvelope(
		_ data: Data,
		expectedProtocolVersion: Int = 1,
		maximumMessages: Int = maximumMessages,
		maximumTextBytes: Int = maximumTextBytes
	) throws -> HMuxConversation {
		guard data.count <= maximumEncodedResponseBytes else {
			throw HMuxConversationDecodeError.textLimitExceeded
		}
		let decoder = JSONDecoder()
		decoder.keyDecodingStrategy = .convertFromSnakeCase
		let envelope = try decoder.decode(HMuxConversationEnvelope.self, from: data)
		guard envelope.appProtocolVersion == expectedProtocolVersion,
		      envelope.ok,
		      envelope.error == nil,
		      let conversation = envelope.data else {
			throw HMuxConversationDecodeError.invalidEnvelope
		}
		try validate(
			sessionID: conversation.sessionID,
			createdAt: conversation.createdAt,
			messages: conversation.messages,
			maximumMessages: maximumMessages,
			maximumTextBytes: maximumTextBytes
		)
		return conversation
	}

	private static func validate(
		sessionID: String,
		createdAt: Int64,
		messages: [Message],
		maximumMessages: Int,
		maximumTextBytes: Int
	) throws {
		guard maximumMessages >= 0, maximumTextBytes >= 0,
		      (1...128).contains(sessionID.utf8.count), createdAt > 0 else {
			throw HMuxConversationDecodeError.invalidConversation
		}
		guard messages.count <= maximumMessages else {
			throw HMuxConversationDecodeError.tooManyMessages
		}

		var messageIDs = Set<String>()
		var textBytes = 0
		for message in messages {
			guard (1...256).contains(message.id.utf8.count),
			      messageIDs.insert(message.id).inserted,
			      message.text.utf8.count <= maximumMessageTextBytes else {
				throw HMuxConversationDecodeError.invalidConversation
			}
			let (nextTotal, overflow) = textBytes.addingReportingOverflow(message.text.utf8.count)
			guard !overflow, nextTotal <= maximumTextBytes else {
				throw HMuxConversationDecodeError.textLimitExceeded
			}
			textBytes = nextTotal
		}
	}
}

private struct HMuxConversationEnvelope: Decodable {
	let appProtocolVersion: Int
	let ok: Bool
	let data: HMuxConversation?
	let error: HMuxConversationErrorPayload?
}

private struct HMuxConversationErrorPayload: Decodable {
	let code: String
	let message: String
}

struct HMuxConversationDisplayMessage: Equatable, Identifiable, Sendable {
	let id: String
	let role: HMuxConversation.Role
	let text: String
}

func hmuxConversationDisplayMessages(
	_ conversation: HMuxConversation,
	showQuestions: Bool,
	showCode: Bool,
	query: String
) -> [HMuxConversationDisplayMessage] {
	let terms = query
		.split(whereSeparator: \.isWhitespace)
		.map { String($0).folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current) }

	return conversation.messages.compactMap { message in
		guard showQuestions || message.role == .assistant else { return nil }
		let text = showCode ? message.text : hmuxConversationRemovingFencedCode(from: message.text)
		let searchable = text.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
		guard terms.allSatisfy(searchable.contains) else { return nil }
		return HMuxConversationDisplayMessage(id: message.id, role: message.role, text: text)
	}
}

func hmuxConversationRemovingFencedCode(from text: String) -> String {
	var output: [Substring] = []
	var fenceCharacter: Character?
	var fenceLength = 0

	for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
		let prefixTrimmed = line.drop(while: { $0 == " " }).prefix(line.count)
		let indentation = line.count - prefixTrimmed.count
		let candidate = indentation <= 3 ? prefixTrimmed : line[line.startIndex...]

		if let activeCharacter = fenceCharacter {
			if let run = hmuxConversationFenceRun(in: candidate),
			   run.character == activeCharacter,
			   run.count >= fenceLength,
			   candidate.dropFirst(run.count).allSatisfy({ $0 == " " || $0 == "\t" }) {
				fenceCharacter = nil
				fenceLength = 0
			}
			continue
		}

		if let run = hmuxConversationFenceRun(in: candidate), run.count >= 3 {
			fenceCharacter = run.character
			fenceLength = run.count
			continue
		}
		output.append(line)
	}

	while output.last?.isEmpty == true { output.removeLast() }
	return output.joined(separator: "\n")
}

private func hmuxConversationFenceRun(in line: Substring) -> (character: Character, count: Int)? {
	guard let character = line.first, character == "`" || character == "~" else { return nil }
	let count = line.prefix(while: { $0 == character }).count
	return (character, count)
}

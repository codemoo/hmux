import Darwin
import Foundation

private enum HMuxCatalogStreamSmokeError: Error {
    case timedOut
}

@main
struct HMuxCatalogStreamSmoke {
    static func main() async throws {
        guard CommandLine.arguments.count == 2 else {
            throw HMuxBackendError.commandFailed
        }
        let helper = CommandLine.arguments[1]
        guard helper.hasPrefix("/") else {
            throw HMuxBackendError.executableUnavailable
        }
        setenv("HMUX_EXECUTABLE", helper, 1)

		let connection = try await HMuxCatalogStreamConnection.open()
        defer {
            Task { await connection.close() }
        }

        let catalog = try await withThrowingTaskGroup(of: HMuxCatalog.self) { group in
            group.addTask { try await connection.receiveCatalog() }
            group.addTask {
                try await Task.sleep(nanoseconds: 15_000_000_000)
                throw HMuxCatalogStreamSmokeError.timedOut
            }
            let result = try await group.next()!
            group.cancelAll()
            return result
        }
        guard catalog.protocolVersion == HMuxBackend.backendProtocolVersion else {
            throw HMuxBackendError.invalidProtocol
        }
        await connection.close()
        print("catalog-stream-ok sessions=\(catalog.sessions.count)")
    }
}

import Foundation
import Testing
import ZeusCore

@Test
func decodesTypedServerEvents() throws {
    let event = try decodeEvent(
        #"{"type":"text_delta","session_id":42,"delta":"hello"}"#
    )

    #expect(event == .textDelta(sessionID: 42, delta: "hello"))
    #expect(event.isAssistantOutputEvent)
}

@Test
func decodesUnknownServerEvents() throws {
    let event = try decodeEvent(
        #"{"type":"new_event","session_id":7,"message":"later"}"#
    )

    #expect(event == .unknown(type: "new_event", sessionID: 7))
}

@Test
func decodesCacheHealthUsage() throws {
    let event = try decodeEvent(
        #"{"type":"cache_health","session_id":1,"cache":{"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}}}"#
    )

    guard case let .cacheHealth(_, cache) = event else {
        Issue.record("Expected cache health event")
        return
    }

    #expect(cache?.usage?.inputTokens == 2)
    #expect(cache?.usage?.outputTokens == 3)
    #expect(cache?.usage?.totalTokens == 5)
}

private func decodeEvent(_ json: String) throws -> AgentServerEvent {
    try JSONDecoder().decode(AgentServerEvent.self, from: Data(json.utf8))
}

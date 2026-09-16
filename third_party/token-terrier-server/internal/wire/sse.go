package wire

import (
	"encoding/json"
	"fmt"
)

// SSEEvent is one serialized server-sent event frame as a string ready to
// write to the response body.
type SSEEvent struct {
	Text string
}

// HeartbeatEvent is the empty-comment frame used to keep idle connections
// alive without consuming a sequence number.
func HeartbeatEvent() SSEEvent {
	return SSEEvent{Text: ":\n\n"}
}

// SnapshotEvent serializes the snapshot as a `snapshot` named event whose
// `id:` line is the snapshot's monotonic seq.
func SnapshotEvent(snapshot UsageSnapshot) (SSEEvent, error) {
	data, err := json.Marshal(snapshot)
	if err != nil {
		return SSEEvent{}, fmt.Errorf("encode snapshot: %w", err)
	}
	return SSEEvent{
		Text: fmt.Sprintf("id: %d\nevent: snapshot\ndata: %s\n\n", snapshot.Seq, data),
	}, nil
}

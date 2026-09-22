package catalogstream

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

const (
	ProtocolVersion = 1
	DefaultInterval = 5 * time.Second
	SnapshotTimeout = 10 * time.Second
	HeartbeatEvery  = 15 * time.Second
)

type SourceFrame struct {
	StreamProtocolVersion int           `json:"stream_protocol_version"`
	Sequence              uint64        `json:"sequence"`
	Type                  string        `json:"type"`
	Catalog               model.Catalog `json:"catalog"`
}

type Fetch func(context.Context) (model.Catalog, error)
type Publish func(SourceFrame) error

// Produce emits an initial full catalog, semantic changes, and catalog-free
// heartbeats. The generated_at timestamp is deliberately excluded from change
// detection. Heartbeats describe successful unchanged polls; Home owns the
// independent WebSocket heartbeat and connection lifetime.
func Produce(ctx context.Context, interval time.Duration, fetch Fetch, publish Publish) error {
	if interval <= 0 || fetch == nil || publish == nil {
		return errors.New("invalid catalog stream producer")
	}
	var sequence uint64
	var previous [sha256.Size]byte
	havePrevious := false
	unchangedPolls := 0
	poll := func() error {
		fetchCtx, cancel := context.WithTimeout(ctx, SnapshotTimeout)
		catalog, err := fetch(fetchCtx)
		cancel()
		if err != nil {
			return err
		}
		digest, err := SemanticDigest(catalog)
		if err != nil {
			return err
		}
		if havePrevious && digest == previous {
			unchangedPolls++
			if unchangedPolls < int(HeartbeatEvery/DefaultInterval) {
				return nil
			}
			sequence++
			if err := publish(SourceFrame{
				StreamProtocolVersion: ProtocolVersion,
				Sequence:              sequence,
				Type:                  "heartbeat",
			}); err != nil {
				return err
			}
			unchangedPolls = 0
			return nil
		}
		sequence++
		if err := publish(SourceFrame{
			StreamProtocolVersion: ProtocolVersion,
			Sequence:              sequence,
			Type:                  "snapshot",
			Catalog:               catalog,
		}); err != nil {
			return err
		}
		previous = digest
		havePrevious = true
		unchangedPolls = 0
		return nil
	}

	if err := poll(); err != nil {
		return err
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			if err := poll(); err != nil {
				return err
			}
		}
	}
}

func SemanticDigest(value model.Catalog) ([sha256.Size]byte, error) {
	value.GeneratedAt = time.Time{}
	data, err := json.Marshal(value)
	if err != nil {
		return [sha256.Size]byte{}, fmt.Errorf("encode catalog digest: %w", err)
	}
	return sha256.Sum256(data), nil
}

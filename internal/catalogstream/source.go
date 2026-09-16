package catalogstream

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

const (
	ProtocolVersion  = 1
	MaximumFrameSize = 32 * 1024 * 1024
	DefaultInterval  = 5 * time.Second
	SnapshotTimeout  = 10 * time.Second
	HeartbeatEvery   = 15 * time.Second
	// A fetch may consume SnapshotTimeout and the next unchanged poll may need
	// several producer intervals before its heartbeat is emitted. Keep the
	// consumer lease comfortably beyond that worst-case healthy cadence.
	SourceLease = 50 * time.Second
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
// detection. Heartbeats prove the producer/SSH path is alive without causing
// a SwiftUI catalog publication.
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

func WriteFrame(writer io.Writer, value SourceFrame) error {
	if value.StreamProtocolVersion != ProtocolVersion || value.Sequence < 1 ||
		(value.Type != "snapshot" && value.Type != "heartbeat") ||
		(value.Type == "snapshot" && value.Catalog.ProtocolVersion != model.ProtocolVersion) ||
		(value.Type == "heartbeat" && value.Catalog.ProtocolVersion != 0) {
		return errors.New("invalid catalog stream frame")
	}
	data, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("encode catalog stream frame: %w", err)
	}
	if len(data) < 1 || len(data) > MaximumFrameSize {
		return errors.New("catalog stream frame exceeds size limit")
	}
	var header [4]byte
	binary.BigEndian.PutUint32(header[:], uint32(len(data)))
	if err := writeAll(writer, header[:]); err != nil {
		return err
	}
	return writeAll(writer, data)
}

func ReadFrame(reader io.Reader) (SourceFrame, error) {
	var header [4]byte
	if _, err := io.ReadFull(reader, header[:]); err != nil {
		return SourceFrame{}, err
	}
	size := binary.BigEndian.Uint32(header[:])
	if size < 1 || size > MaximumFrameSize {
		return SourceFrame{}, errors.New("catalog stream frame has invalid size")
	}
	data := make([]byte, int(size))
	if _, err := io.ReadFull(reader, data); err != nil {
		return SourceFrame{}, fmt.Errorf("read catalog stream frame: %w", err)
	}
	if err := model.ValidateCatalogJSONStructure(data); err != nil {
		return SourceFrame{}, err
	}
	var value SourceFrame
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&value); err != nil {
		return SourceFrame{}, fmt.Errorf("decode catalog stream frame: %w", err)
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return SourceFrame{}, errors.New("catalog stream frame contains trailing data")
	}
	if value.StreamProtocolVersion != ProtocolVersion {
		return SourceFrame{}, errors.New("catalog stream protocol mismatch")
	}
	if value.Sequence < 1 || (value.Type != "snapshot" && value.Type != "heartbeat") ||
		(value.Type == "snapshot" && value.Catalog.ProtocolVersion != model.ProtocolVersion) ||
		(value.Type == "heartbeat" && value.Catalog.ProtocolVersion != 0) {
		return SourceFrame{}, errors.New("catalog stream frame metadata is invalid")
	}
	return value, nil
}

func writeAll(writer io.Writer, data []byte) error {
	for len(data) > 0 {
		written, err := writer.Write(data)
		if err != nil {
			return err
		}
		if written < 1 || written > len(data) {
			return io.ErrShortWrite
		}
		data = data[written:]
	}
	return nil
}

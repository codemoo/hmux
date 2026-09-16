package catalogstream

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"io"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

func TestProduceSuppressesGeneratedAtOnlyChanges(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var fetches atomic.Int32
	var frames []SourceFrame
	err := Produce(ctx, time.Millisecond, func(context.Context) (model.Catalog, error) {
		count := fetches.Add(1)
		if count >= 4 {
			cancel()
		}
		return model.Catalog{
			ProtocolVersion: model.ProtocolVersion,
			GeneratedAt:     time.Unix(int64(count), 0),
			Sessions:        []model.Session{{ID: "$1", CreatedAt: 1, Name: "stable"}},
		}, nil
	}, func(frame SourceFrame) error {
		frames = append(frames, frame)
		return nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("producer error=%v", err)
	}
	if len(frames) < 2 || frames[0].Sequence != 1 || frames[0].Type != "snapshot" {
		t.Fatalf("unchanged frames=%#v", frames)
	}
	for _, frame := range frames[1:] {
		if frame.Type != "heartbeat" || frame.Catalog.ProtocolVersion != 0 {
			t.Fatalf("invalid heartbeat=%#v", frame)
		}
	}
}

func TestSourceLeaseCoversSlowHealthyHeartbeatCadence(t *testing.T) {
	unchangedPollsPerHeartbeat := HeartbeatEvery / DefaultInterval
	worstHealthyCadence := DefaultInterval + unchangedPollsPerHeartbeat*SnapshotTimeout
	if SourceLease <= worstHealthyCadence {
		t.Fatalf("source lease %v does not cover slow healthy cadence %v", SourceLease, worstHealthyCadence)
	}
}

func TestFrameHeartbeatRoundTrip(t *testing.T) {
	frame := SourceFrame{StreamProtocolVersion: ProtocolVersion, Sequence: 9, Type: "heartbeat"}
	var encoded bytes.Buffer
	if err := WriteFrame(&encoded, frame); err != nil {
		t.Fatal(err)
	}
	decoded, err := ReadFrame(&encoded)
	if err != nil || decoded.Type != "heartbeat" || decoded.Sequence != 9 || decoded.Catalog.ProtocolVersion != 0 {
		t.Fatalf("decoded=%#v err=%v", decoded, err)
	}
}

func TestProduceSequencesSemanticChanges(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var fetches atomic.Int32
	var sequences []uint64
	err := Produce(ctx, time.Millisecond, func(context.Context) (model.Catalog, error) {
		count := fetches.Add(1)
		if count >= 3 {
			cancel()
		}
		return model.Catalog{
			ProtocolVersion: model.ProtocolVersion,
			Sessions:        []model.Session{{ID: "$1", CreatedAt: 1, Name: string(rune('a' + count))}},
		}, nil
	}, func(frame SourceFrame) error {
		sequences = append(sequences, frame.Sequence)
		return nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("producer error=%v", err)
	}
	if len(sequences) != 3 || sequences[0] != 1 || sequences[1] != 2 || sequences[2] != 3 {
		t.Fatalf("sequences=%v", sequences)
	}
}

func TestLengthPrefixedFrameRoundTripAndBounds(t *testing.T) {
	frame := SourceFrame{
		StreamProtocolVersion: ProtocolVersion,
		Sequence:              7,
		Type:                  "snapshot",
		Catalog: model.Catalog{
			ProtocolVersion: model.ProtocolVersion,
			Sessions:        []model.Session{{ID: "$9", CreatedAt: 9, Name: "한글"}},
		},
	}
	var encoded bytes.Buffer
	if err := WriteFrame(&encoded, frame); err != nil {
		t.Fatal(err)
	}
	decoded, err := ReadFrame(&encoded)
	if err != nil || decoded.Sequence != 7 || decoded.Catalog.Sessions[0].Name != "한글" {
		t.Fatalf("decoded=%#v err=%v", decoded, err)
	}

	var oversized [4]byte
	binary.BigEndian.PutUint32(oversized[:], MaximumFrameSize+1)
	if _, err := ReadFrame(bytes.NewReader(oversized[:])); err == nil {
		t.Fatal("oversized frame was accepted")
	}
	var truncated bytes.Buffer
	binary.Write(&truncated, binary.BigEndian, uint32(10))
	truncated.WriteString("{}")
	if _, err := ReadFrame(&truncated); err == nil || !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("truncated frame error=%v", err)
	}
}

func TestReadFrameRejectsMalformedTrailingAndUnknownProtocol(t *testing.T) {
	encode := func(data []byte) []byte {
		var framed bytes.Buffer
		if err := binary.Write(&framed, binary.BigEndian, uint32(len(data))); err != nil {
			t.Fatal(err)
		}
		framed.Write(data)
		return framed.Bytes()
	}
	for name, data := range map[string][]byte{
		"malformed": []byte(`{"stream_protocol_version":1`),
		"trailing":  []byte(`{"stream_protocol_version":1,"sequence":1,"type":"snapshot","catalog":{"protocol_version":1,"generated_at":"0001-01-01T00:00:00Z","sessions":null}} {}`),
		"unknown":   []byte(`{"stream_protocol_version":2,"sequence":1,"type":"snapshot","catalog":{"protocol_version":1,"generated_at":"0001-01-01T00:00:00Z","sessions":null}}`),
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := ReadFrame(bytes.NewReader(encode(data))); err == nil {
				t.Fatalf("%s frame was accepted", name)
			}
		})
	}
}

package catalogstream

import (
	"context"
	"errors"
	"fmt"
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

func TestProduceWithRefreshPollsWithoutWaitingForInterval(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	refresh := make(chan struct{}, 1)
	var fetches atomic.Int32
	published := make(chan SourceFrame, 4)
	done := make(chan error, 1)
	go func() {
		// An hour-long interval proves the second poll came from refresh.
		done <- ProduceWithRefresh(ctx, time.Hour, refresh, func(context.Context) (model.Catalog, error) {
			count := fetches.Add(1)
			return model.Catalog{
				ProtocolVersion: model.ProtocolVersion,
				Sessions:        []model.Session{{ID: "$1", CreatedAt: 1, Name: fmt.Sprintf("s%d", count)}},
			}, nil
		}, func(frame SourceFrame) error {
			published <- frame
			return nil
		})
	}()
	first := <-published
	refresh <- struct{}{}
	select {
	case second := <-published:
		if first.Sequence != 1 || second.Sequence != 2 || second.Type != "snapshot" || second.Catalog.Sessions[0].Name != "s2" {
			t.Fatalf("frames = %#v %#v", first, second)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("refresh did not trigger an immediate poll")
	}
	cancel()
	if err := <-done; !errors.Is(err, context.Canceled) {
		t.Fatalf("producer error = %v", err)
	}
}

package sse

import (
	"context"
	"math"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestCloseAndWaitTerminatesSubscribers(t *testing.T) {
	hub := NewHub()
	events, _ := hub.Subscribe(context.Background())
	if got := hub.ClientCount(); got != 1 {
		t.Fatalf("client count = %d, want 1", got)
	}

	hub.Close()
	hub.Wait()
	if got := hub.ClientCount(); got != 0 {
		t.Fatalf("client count after Close = %d, want 0", got)
	}
	if _, open := <-events; open {
		t.Fatal("subscriber channel remains open after Close and Wait")
	}

	afterClose, _ := hub.Subscribe(context.Background())
	if _, open := <-afterClose; open {
		t.Fatal("subscription admitted after hub Close")
	}
}

func TestPublishSnapshotCountsEncodeFailure(t *testing.T) {
	hub := NewHub()
	err := hub.PublishSnapshot(wire.UsageSnapshot{Schema: 1, Seq: 1, BurnRatePerMinute: math.NaN()})
	if err == nil {
		t.Fatal("non-finite snapshot unexpectedly encoded")
	}
	if got := hub.EncodeFailures(); got != 1 {
		t.Fatalf("encode failures = %d, want 1", got)
	}
}

func TestHeartbeatNeverEvictsPendingSnapshot(t *testing.T) {
	hub := NewHub()
	hub.heartbeatInterval = time.Millisecond
	events, cancel := hub.Subscribe(context.Background())
	defer func() {
		cancel()
		hub.Close()
		hub.Wait()
	}()

	if err := hub.PublishSnapshot(wire.UsageSnapshot{Schema: 1, Seq: 7}); err != nil {
		t.Fatal(err)
	}
	time.Sleep(5 * time.Millisecond)
	select {
	case event := <-events:
		if !strings.Contains(event.Text, "event: snapshot") || !strings.Contains(event.Text, "id: 7") {
			t.Fatalf("pending snapshot was displaced: %q", event.Text)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for snapshot")
	}
}

func TestSnapshotReplacesQueuedHeartbeat(t *testing.T) {
	hub := NewHub()
	hub.heartbeatInterval = time.Millisecond
	events, cancel := hub.Subscribe(context.Background())
	defer func() {
		cancel()
		hub.Close()
		hub.Wait()
	}()

	time.Sleep(5 * time.Millisecond)
	if err := hub.PublishSnapshot(wire.UsageSnapshot{Schema: 1, Seq: 8}); err != nil {
		t.Fatal(err)
	}
	select {
	case event := <-events:
		if !strings.Contains(event.Text, "event: snapshot") || !strings.Contains(event.Text, "id: 8") {
			t.Fatalf("snapshot did not replace heartbeat: %q", event.Text)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for snapshot")
	}
}

func TestPublishSnapshotDropsDuplicateAndRegressingSequences(t *testing.T) {
	hub := NewHub()
	events, cancel := hub.Subscribe(context.Background())
	defer func() {
		cancel()
		hub.Close()
		hub.Wait()
	}()

	if err := hub.PublishSnapshot(wire.UsageSnapshot{Schema: 1, Seq: 10}); err != nil {
		t.Fatal(err)
	}
	<-events
	for _, seq := range []int{10, 9} {
		if err := hub.PublishSnapshot(wire.UsageSnapshot{Schema: 1, Seq: seq}); err != nil {
			t.Fatal(err)
		}
	}
	select {
	case event := <-events:
		t.Fatalf("duplicate/regressing snapshot was published: %q", event.Text)
	case <-time.After(20 * time.Millisecond):
	}

	if err := hub.PublishSnapshot(wire.UsageSnapshot{Schema: 1, Seq: 11}); err != nil {
		t.Fatal(err)
	}
	select {
	case event := <-events:
		if !strings.Contains(event.Text, "id: 11") {
			t.Fatalf("newer snapshot missing: %q", event.Text)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for newer snapshot")
	}
}

package webgateway

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/model"
)

func TestCompletionDiscoveryDoesNotBlockCatalogAndCoalescesSnapshots(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	entered := make(chan string, 2)
	release := make(chan struct{})
	var calls atomic.Int32
	w := newCompletionWorker(func(ctx context.Context, value model.Catalog) ([]catalog.TaskCompletion, error) {
		entered <- value.Sessions[0].ID
		if calls.Add(1) == 1 {
			select {
			case <-release:
			case <-ctx.Done():
			}
		}
		return nil, nil
	}, func(context.Context, Message) error { t.Error("unexpected send"); return nil })
	done := make(chan struct{})
	go func() { defer close(done); w.run(ctx) }()
	t.Cleanup(func() { cancel(); <-done })
	first := model.Catalog{Sessions: []model.Session{{ID: "$1", CreatedAt: 1}}}
	if err := w.enqueue(ctx, first); err != nil {
		t.Fatal(err)
	}
	select {
	case <-entered:
	case <-time.After(time.Second):
		t.Fatal("observer did not start")
	}
	// The observer remains blocked while catalog production continues. Copying
	// the slice also prevents later producer edits from changing queued input.
	queued := make(chan struct{})
	go func() {
		defer close(queued)
		for _, id := range []string{"$2", "$3", "$4"} {
			value := model.Catalog{Sessions: []model.Session{{ID: id, CreatedAt: 1}}}
			_ = w.enqueue(ctx, value)
			value.Sessions[0].ID = "$99"
		}
	}()
	select {
	case <-queued:
	case <-time.After(time.Second):
		t.Fatal("catalog publication blocked on completion discovery")
	}
	if len(w.pending) != 1 {
		t.Fatal("pending catalog queue is not bounded")
	}
	close(release)
	select {
	case id := <-entered:
		if id != "$4" {
			t.Fatalf("expected latest detached snapshot, got %s", id)
		}
	case <-time.After(time.Second):
		t.Fatal("latest snapshot not observed")
	}
}

func TestCompletionFailuresDoNotCancelConnectorOrStopFutureObservation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var calls atomic.Int32
	sent := make(chan struct{}, 2)
	observed := make(chan struct{}, 3)
	w := newCompletionWorker(func(context.Context, model.Catalog) ([]catalog.TaskCompletion, error) {
		defer func() { observed <- struct{}{} }()
		if calls.Add(1) == 1 {
			return nil, errors.New("discovery failed")
		}
		return []catalog.TaskCompletion{{EventID: "synthetic"}}, nil
	}, func(context.Context, Message) error {
		sent <- struct{}{}
		return errors.New("send failed")
	})
	done := make(chan struct{})
	go func() { defer close(done); w.run(ctx) }()
	t.Cleanup(func() { cancel(); <-done })
	for i := 0; i < 3; i++ {
		_ = w.enqueue(ctx, model.Catalog{})
		select {
		case <-observed:
		case <-time.After(time.Second):
			t.Fatal("worker stopped after notification failure")
		}
		if i > 0 {
			select {
			case <-sent:
			case <-time.After(time.Second):
				t.Fatal("notification not attempted")
			}
		}
		if ctx.Err() != nil {
			t.Fatal("notification failure cancelled connector")
		}
	}
}

func TestPeerWriterWaitHonorsCancellation(t *testing.T) {
	p := &peer{} // No socket or real Home required: all sends must stop at the gate.
	if err := p.acquireWriter(context.Background()); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	result := make(chan error, 1)
	go func() { result <- p.send(ctx, Message{Type: "task-complete"}) }()
	cancel()
	select {
	case err := <-result:
		if !errors.Is(err, context.Canceled) {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("cancelled send waited on another writer")
	}
	if len(p.writeLock) != 1 {
		t.Fatal("cancelled waiter released another writer")
	}
	<-p.writeLock
	// Already-expired requests must not write even when the gate is available.
	if err := p.send(ctx, Message{Type: "catalog"}); !errors.Is(err, context.Canceled) {
		t.Fatal(err)
	}
	if err := p.acquireWriter(context.Background()); err != nil {
		t.Fatal(err)
	}
	<-p.writeLock
}

func TestCompletionWorkerCancellationStopsDiscoveryAndCooldown(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	entered := make(chan struct{})
	w := newCompletionWorker(func(ctx context.Context, _ model.Catalog) ([]catalog.TaskCompletion, error) {
		close(entered)
		<-ctx.Done()
		return nil, ctx.Err()
	}, func(context.Context, Message) error { t.Error("cancelled worker sent an event"); return nil })
	done := make(chan struct{})
	go func() { defer close(done); w.run(ctx) }()
	_ = w.enqueue(ctx, model.Catalog{})
	select {
	case <-entered:
	case <-time.After(time.Second):
		t.Fatal("observer did not start")
	}
	_ = w.enqueue(ctx, model.Catalog{})
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("cancellation blocked on discovery or cooldown")
	}
}

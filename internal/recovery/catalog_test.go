package recovery

import (
	"context"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

type blockingCheckpoint struct {
	started chan struct{}
	calls   atomic.Int32
}

func (s *blockingCheckpoint) Save(ctx context.Context) error {
	s.calls.Add(1)
	select {
	case s.started <- struct{}{}:
	default:
	}
	<-ctx.Done()
	return ctx.Err()
}

func TestCatalogCheckpointIsIndependentAndJoined(t *testing.T) {
	checkpoint := &blockingCheckpoint{started: make(chan struct{}, 1)}
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	closeWorker := startCatalogCheckpoint(ctx, checkpoint, time.Millisecond)
	select {
	case <-checkpoint.started:
	case <-time.After(time.Second):
		t.Fatal("checkpoint did not start")
	}
	time.Sleep(20 * time.Millisecond)
	if checkpoint.calls.Load() != 1 {
		t.Fatal("checkpoint calls overlapped")
	}
	closed := make(chan struct{})
	go func() { closeWorker(); close(closed) }()
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("checkpoint worker did not join")
	}
}

func TestApplyReadsCommittedStateWhileCheckpointHoldsLock(t *testing.T) {
	store := Store{StateDir: filepath.Join(t.TempDir(), "state")}
	if _, err := store.ensureRoot(); err != nil {
		t.Fatal(err)
	}
	if err := store.writeState(recoveryTestState()); err != nil {
		t.Fatal(err)
	}
	locked := make(chan struct{})
	release := make(chan struct{})
	defer close(release)
	go func() { _ = store.withLock(t.Context(), func() error { close(locked); <-release; return nil }) }()
	select {
	case <-locked:
	case <-time.After(time.Second):
		t.Fatal("lock not acquired")
	}
	finished := make(chan error, 1)
	go func() { finished <- store.Apply(&model.Catalog{}) }()
	select {
	case err := <-finished:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("Apply waited for checkpoint lock")
	}
}

func TestApplyRejectsUnsafeRecoveryRoot(t *testing.T) {
	parent := t.TempDir()
	store := Store{StateDir: filepath.Join(parent, "state")}
	if err := store.Apply(&model.Catalog{}); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(store.root(), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(&model.Catalog{}); err == nil {
		t.Fatal("unsafe recovery root accepted")
	}
}

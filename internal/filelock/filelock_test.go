package filelock

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"
)

func TestAcquireHonorsContextAndTimeout(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.lock")
	first, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	if err := syscall.Flock(int(first.Fd()), syscall.LOCK_EX); err != nil {
		t.Fatal(err)
	}
	defer Unlock(first)

	second, err := os.OpenFile(path, os.O_RDWR, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer second.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 80*time.Millisecond)
	defer cancel()
	started := time.Now()
	err = Acquire(ctx, second, time.Second)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("error=%v", err)
	}
	if elapsed := time.Since(started); elapsed > 500*time.Millisecond {
		t.Fatalf("context lock acquisition took %v", elapsed)
	}

	started = time.Now()
	err = Acquire(context.Background(), second, 60*time.Millisecond)
	if !errors.Is(err, ErrBusy) {
		t.Fatalf("error=%v", err)
	}
	if elapsed := time.Since(started); elapsed > 500*time.Millisecond {
		t.Fatalf("bounded lock acquisition took %v", elapsed)
	}
}

func TestAcquireSucceedsAfterRelease(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.lock")
	first, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	if err := syscall.Flock(int(first.Fd()), syscall.LOCK_EX); err != nil {
		t.Fatal(err)
	}

	second, err := os.OpenFile(path, os.O_RDWR, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	defer second.Close()
	done := make(chan struct{})
	go func() {
		defer close(done)
		time.Sleep(60 * time.Millisecond)
		Unlock(first)
	}()
	if err := Acquire(context.Background(), second, time.Second); err != nil {
		t.Fatal(err)
	}
	Unlock(second)
	<-done
}

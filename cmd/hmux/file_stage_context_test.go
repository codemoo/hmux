package main

import (
	"context"
	"sync/atomic"
	"testing"
	"time"
)

func TestFileStageAppContextRejectsMismatchedDirectParent(t *testing.T) {
	t.Setenv("HMUX_FILE_STAGE_PARENT_PID", "1234")
	ctx, stop, err := fileStageAppContext(context.Background(), func() int { return 4321 })
	if ctx != nil {
		t.Fatal("mismatched parent returned a context")
	}
	if stop == nil {
		t.Fatal("mismatched parent returned no cleanup function")
	}
	if err == nil || err.Error() != "HMUX_FILE_STAGE_PARENT_PID does not match the direct app parent" {
		t.Fatalf("error=%v", err)
	}
}

func TestFileStageAppContextCancelsWhenDirectParentChanges(t *testing.T) {
	t.Setenv("HMUX_FILE_STAGE_PARENT_PID", "1234")
	previousInterval := fileStageParentPollInterval
	fileStageParentPollInterval = 10 * time.Millisecond
	t.Cleanup(func() { fileStageParentPollInterval = previousInterval })
	var parentPID atomic.Int64
	parentPID.Store(1234)
	ctx, stop, err := fileStageAppContext(context.Background(), func() int { return int(parentPID.Load()) })
	if err != nil {
		t.Fatal(err)
	}
	defer stop()
	parentPID.Store(4321)
	select {
	case <-ctx.Done():
	case <-time.After(time.Second):
		t.Fatal("file-stage context did not observe parent exit")
	}
}

func TestFileStageAppContextAllowsDirectInvocationWithoutParentBinding(t *testing.T) {
	t.Setenv("HMUX_FILE_STAGE_PARENT_PID", "")
	ctx, stop, err := fileStageAppContext(context.Background(), func() int { return 1 })
	if err != nil {
		t.Fatal(err)
	}
	defer stop()
	select {
	case <-ctx.Done():
		t.Fatalf("unbound context was canceled: %v", ctx.Err())
	default:
	}
}

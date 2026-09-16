package main

import (
	"context"
	"sync/atomic"
	"testing"
	"time"
)

func TestAppChildrenObserveParentDeath(t *testing.T) {
	for _, key := range []string{"HMUX_CATALOG_PARENT_PID", "HMUX_USAGE_PARENT_PID", "HMUX_FILE_STAGE_PARENT_PID"} {
		t.Run(key, func(t *testing.T) {
			var pid atomic.Int64
			pid.Store(1234)
			ctx, stop, err := appChildContext(context.Background(), key, func(name string) string {
				if name != key {
					t.Errorf("unexpected key %s", name)
				}
				return "1234"
			}, func() int { return int(pid.Load()) }, time.Millisecond)
			if err != nil {
				t.Fatal(err)
			}
			defer stop()
			pid.Store(1)
			select {
			case <-ctx.Done():
			case <-time.After(time.Second):
				t.Fatal("orphaned helper remained active")
			}
		})
	}
}

func TestCatalogChildRejectsInvalidParentBeforeStartingTransport(t *testing.T) {
	for _, raw := range []string{"1", "-1", "other", "1235"} {
		t.Setenv("HMUX_CATALOG_PARENT_PID", raw)
		ctx, stop, err := catalogAppParentContext(context.Background(), func() int { return 1234 })
		stop()
		if err == nil || ctx != nil {
			t.Fatalf("accepted parent %q", raw)
		}
	}
}

func TestAppChildUnboundInvocationStillObservesCancellation(t *testing.T) {
	parent, cancel := context.WithCancel(context.Background())
	ctx, stop, err := appChildContext(parent, "parent", func(string) string { return "" }, nil, time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	defer stop()
	if ctx.Err() != nil {
		t.Fatal("unbound CLI invocation was rejected")
	}
	cancel()
	select {
	case <-ctx.Done():
	case <-time.After(time.Second):
		t.Fatal("caller cancellation was lost")
	}
}

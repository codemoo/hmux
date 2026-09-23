package webgateway

import (
	"context"
	"errors"
	"fmt"
	"net"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/timing"
)

func TestTransportTraceOmitsSensitiveErrorContents(t *testing.T) {
	var logs []string
	var mu sync.Mutex
	p := observedPeer(&peer{}, func(s string) { mu.Lock(); defer mu.Unlock(); logs = append(logs, s) })
	var wg sync.WaitGroup
	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			p.trace("transport-write", fmt.Errorf("SECRET-token private-path: %w", net.ErrClosed))
		}()
	}
	wg.Wait()
	for _, s := range logs {
		if strings.Contains(s, "SECRET") || strings.Contains(s, "private-path") || !strings.Contains(s, "socket-closed") || !strings.Contains(s, "connection=") {
			t.Fatal(s)
		}
	}
}
func TestRequestLogDoesNotIncludeUntrustedOperationOrIdentity(t *testing.T) {
	var logs []string
	h := newHub()
	h.report = func(s string) { logs = append(logs, s) }
	_, err := h.request(context.Background(), Message{Type: "request", Operation: "SECRET-operation", ID: "SECRET-id"})
	if err == nil || len(logs) != 1 || strings.Contains(logs[0], "SECRET") || !strings.Contains(logs[0], "request-complete") {
		t.Fatal(logs, err)
	}
}
func TestQueuedWriterDeadlineLeavesTransportUntouched(t *testing.T) {
	p := &peer{}
	if err := p.acquireWriter(context.Background()); err != nil {
		t.Fatal(err)
	}
	defer func() { <-p.writeLock }()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Millisecond)
	defer cancel()
	if err := p.send(ctx, Message{Type: "request"}); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatal(err)
	}
}

func TestLatencyOperationAllowlist(t *testing.T) {
	for _, operation := range []string{"workspace", "conversation", "profiles", "provider-job"} {
		if logOperation(Message{Operation: operation}) != operation {
			t.Fatal(operation)
		}
	}
	if logOperation(Message{Operation: "workspace\nSECRET"}) != "unknown" {
		t.Fatal("unsafe operation logged")
	}
	if logOperation(Message{Type: "open", Operation: "SECRET"}) != "terminal-open" {
		t.Fatal("open classification")
	}
	var logs []string
	p := observedPeer(&peer{}, func(s string) { logs = append(logs, s) })
	ctx := p.timingContext(context.Background(), "conversation")
	timing.Start(ctx, "home-processing", true)()
	if len(logs) != 1 || !strings.Contains(logs[0], "operation=conversation stage=home-processing duration_ms=") {
		t.Fatal(logs)
	}
}

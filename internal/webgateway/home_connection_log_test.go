package webgateway

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"syscall"
	"testing"

	"github.com/codemoo/hmux/internal/config"
	"github.com/coder/websocket"
)

func TestHomeConnectionLogRedactsErrors(t *testing.T) {
	secret := "https://private.example/SECRET-TOKEN /private/credentials terminal text"
	for _, tc := range []struct {
		err  error
		want string
	}{
		{fmt.Errorf("%s: %w", secret, context.DeadlineExceeded), "timeout"},
		{&net.DNSError{Name: secret, Err: secret}, "dns"},
		{fmt.Errorf("%s: %w", secret, syscall.ECONNRESET), "connection-reset"},
		{websocket.CloseError{Code: websocket.StatusPolicyViolation, Reason: secret}, "websocket-close"},
		{errors.New(secret), "internal-or-protocol"},
	} {
		got := homeConnectionSummary(&homeConnectionFailure{stage: "read", cause: tc.err})
		if !strings.Contains(got, "reason="+tc.want) || strings.Contains(got, "private") || strings.Contains(got, "SECRET") || strings.Contains(got, "terminal text") {
			t.Fatal(got)
		}
	}
}
func TestHomeFailurePreservesTriggerDuringConcurrentShutdown(t *testing.T) {
	var r homeFailureRecorder
	r.record("heartbeat", context.DeadlineExceeded)
	var wg sync.WaitGroup
	for i := 0; i < 20; i++ {
		wg.Add(1)
		go func() { defer wg.Done(); r.record("read", context.Canceled); _ = r.result(nil) }()
	}
	wg.Wait()
	if got := homeConnectionSummary(r.result(context.Canceled)); got != "stage=heartbeat reason=timeout" {
		t.Fatal(got)
	}
}
func TestHomeDialLogsHTTPStatusWithoutResponseBody(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "SECRET remote response", http.StatusForbidden)
	}))
	defer server.Close()
	err := connectOnce(context.Background(), "ws"+strings.TrimPrefix(server.URL, "http"), "SECRET-token", config.HomeConfig{})
	if err == nil {
		t.Fatal("expected rejected handshake")
	}
	got := homeConnectionSummary(err)
	if !strings.Contains(got, "stage=dial") || !strings.Contains(got, "http_status=403") || strings.Contains(got, "SECRET") || strings.Contains(got, server.URL) {
		t.Fatal(got)
	}
}

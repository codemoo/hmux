package webgateway

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/filestage"
	"github.com/coder/websocket"
)

// Launched by the Rust ignored HTTP test through make rust-compat. Only the
// tmux identity verifier and private temporary spool root are synthetic; the
// actual Go peer codec, upload worker and fsync/commit receiver are exercised.
func TestRustGatewayActualGoUploadHome(t *testing.T) {
	address := os.Getenv("HMUX_RUST_UPLOAD_ADDRESS")
	if address == "" {
		t.Skip("isolated Rust HTTP candidate supplies a loopback address")
	}
	host, _, err := net.SplitHostPort(address)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		t.Fatal("loopback required")
	}
	parent := os.Getenv("HMUX_RUST_UPLOAD_DIRECTORY")
	if !filepath.IsAbs(parent) || !strings.HasPrefix(filepath.Base(parent), "hmux-e2e-rust-http-auth-") {
		t.Fatal("isolated directory required")
	}
	root := filepath.Join(parent, "hmux", "staged-files-v1")
	originalRoot, originalVerify := homeFileStageRoot, homeFileStageVerify
	defer func() { homeFileStageRoot, homeFileStageVerify = originalRoot, originalVerify }()
	homeFileStageRoot = func() (string, error) { return root, nil }
	var verifies atomic.Int32
	homeFileStageVerify = func(_ context.Context, session filestage.SessionIdentity) error {
		if session.ID != "$7" || session.CreatedAt != 42 {
			return fmt.Errorf("wrong session")
		}
		verifies.Add(1)
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	transport := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "tcp", address)
	}}
	defer transport.CloseIdleConnections()
	conn, _, err := websocket.Dial(ctx, "ws://hmux.example/connect", &websocket.DialOptions{
		HTTPClient: &http.Client{Transport: transport},
		HTTPHeader: http.Header{"Authorization": {"Bearer AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}},
	})
	if err != nil {
		t.Fatal(err)
	}
	defer conn.CloseNow()
	conn.SetReadLimit(maxMessage)
	p := &peer{conn: conn}
	if err := p.send(ctx, Message{Type: "hello", Capabilities: []string{"web-upload-v1"}}); err != nil {
		t.Fatal(err)
	}
	bridgeDone, workerDone := serveActualUploadHome(ctx, p)
	select {
	case <-bridgeDone:
	case <-ctx.Done():
		t.Fatal("Rust gateway did not close the Home connection")
	}
	select {
	case <-workerDone:
	default:
		t.Fatal("Home upload worker did not finish")
	}
	if verifies.Load() != 2 {
		t.Fatalf("session identity checked %d times", verifies.Load())
	}
}

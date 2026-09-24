package webgateway

// Native process rollback uses current synthetic authentication state throughout.
// Never restore an old credentials/session snapshot to make a binary downgrade work.
import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

func TestRustNativeCurrentStateRollback(t *testing.T) {
	rust, baseline := os.Getenv("HMUX_RUST_WEB_BIN"), os.Getenv("HMUX_GO_WEB_BIN")
	if rust == "" || baseline == "" {
		t.Skip("set HMUX_RUST_WEB_BIN and HMUX_GO_WEB_BIN to native executables")
	}
	if !filepath.IsAbs(rust) || !filepath.IsAbs(baseline) {
		t.Fatal("absolute native binary paths required")
	}
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	credentials, err := NewCredentials("synthetic", "synthetic-rollback-password")
	if err != nil {
		t.Fatal(err)
	}
	credentials.TOTPDisabled = true
	if err := WriteCredentials(filepath.Join(root, "credentials.json"), credentials); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "connector.token"), []byte(fullGatewayToken), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(root, "assets"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "assets/index.html"), []byte("synthetic rollback"), 0600); err != nil {
		t.Fatal(err)
	}
	type login struct{ cookie, csrf string }
	var client *http.Client
	var child *exec.Cmd
	var done chan error
	stop := func() {
		t.Helper()
		if child == nil {
			return
		}
		client.CloseIdleConnections()
		_ = child.Process.Signal(syscall.SIGTERM)
		select {
		case err := <-done:
			child = nil
			if err != nil {
				t.Fatalf("native gateway shutdown: %v", err)
			}
		case <-time.After(10 * time.Second):
			_ = child.Process.Kill()
			<-done
			child = nil
			t.Fatal("native gateway did not stop before replacement")
		}
	}
	t.Cleanup(stop)
	start := func(binary, role string) {
		t.Helper()
		if child != nil {
			t.Fatal("must stop old owner before opening current state")
		}
		listener, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		address := listener.Addr().String()
		_ = listener.Close()
		child = exec.Command(binary, "serve", "--origin", "https://hmux.example", "--credentials", filepath.Join(root, "credentials.json"), "--token-file", filepath.Join(root, "connector.token"), "--assets", filepath.Join(root, "assets"), "--listen", address)
		child.Env = []string{"HOME=" + root, "PATH=/usr/bin:/bin", "LANG=C", "TZ=UTC"}
		log, err := os.OpenFile(filepath.Join(root, role+".log"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = log.Close() })
		child.Stdout, child.Stderr = log, log
		if err := child.Start(); err != nil {
			child = nil
			t.Fatal(err)
		}
		done = make(chan error, 1)
		command, completion := child, done
		go func() { completion <- command.Wait() }()
		client = &http.Client{Transport: fullGatewayTransport(address), Timeout: 3 * time.Second}
		deadline := time.Now().Add(15 * time.Second)
		for time.Now().Before(deadline) {
			response, err := client.Get("http://hmux.example/")
			if err == nil {
				_, _ = io.Copy(io.Discard, response.Body)
				_ = response.Body.Close()
				if response.StatusCode == 200 {
					return
				}
			}
			select {
			case err := <-done:
				child = nil
				t.Fatalf("%s exited before readiness: %v", role, err)
			default:
			}
			time.Sleep(20 * time.Millisecond)
		}
		t.Fatalf("%s did not become ready", role)
	}
	request := func(method, path string, session login, value any) (int, http.Header, []byte) {
		t.Helper()
		var body io.Reader
		if value != nil {
			raw, err := json.Marshal(value)
			if err != nil {
				t.Fatal(err)
			}
			body = bytes.NewReader(raw)
		}
		req, err := http.NewRequest(method, "http://hmux.example"+path, body)
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("Origin", "https://hmux.example")
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Cookie", session.cookie)
		req.Header.Set("X-CSRF-Token", session.csrf)
		response, err := client.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		defer response.Body.Close()
		raw, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
		if err != nil {
			t.Fatal(err)
		}
		return response.StatusCode, response.Header, raw
	}
	signIn := func() login {
		t.Helper()
		status, header, _ := request("POST", "/api/login", login{}, map[string]string{"username": "synthetic", "password": "synthetic-rollback-password"})
		if status != 200 || header.Get("Set-Cookie") == "" {
			t.Fatalf("native login status=%d", status)
		}
		session := login{cookie: strings.Split(header.Get("Set-Cookie"), ";")[0]}
		status, _, raw := request("GET", "/api/session", session, nil)
		var payload struct {
			CSRF string `json:"csrf"`
		}
		if status != 200 || json.Unmarshal(raw, &payload) != nil || payload.CSRF == "" {
			t.Fatal("native session/CSRF unavailable")
		}
		session.csrf = payload.CSRF
		return session
	}
	assertAccess := func(session login, allowed bool) {
		t.Helper()
		status, _, _ := request("GET", "/api/state", session, nil)
		want := http.StatusUnauthorized
		if allowed {
			want = http.StatusOK
		}
		if status != want {
			t.Fatalf("native current-state access: got %d want %d", status, want)
		}
	}
	signOut := func(session login) {
		t.Helper()
		status, _, _ := request("POST", "/api/logout", session, nil)
		if status != 200 {
			t.Fatal(fmt.Sprintf("native logout: %d", status))
		}
		assertAccess(session, false)
	}
	start(rust, "rust-before")
	revokedOnRust, retained := signIn(), signIn()
	signOut(revokedOnRust)
	assertAccess(retained, true)
	stop()
	start(baseline, "go-rollback")
	assertAccess(revokedOnRust, false)
	assertAccess(retained, true)
	signOut(retained)
	createdOnGo := signIn()
	stop()
	start(rust, "rust-return")
	assertAccess(revokedOnRust, false)
	assertAccess(retained, false)
	assertAccess(createdOnGo, true)
	signOut(createdOnGo)
	stop()
	t.Log("native Rust -> Go -> Rust preserved current login state and both revocations")
}

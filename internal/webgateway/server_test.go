package webgateway

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha1"
	"encoding/base32"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/coder/websocket"
)

const testPassword = "this-is-only-a-test-password"

func codeAt(c Credentials, at time.Time) string {
	key, _ := base32.StdEncoding.WithPadding(base32.NoPadding).DecodeString(c.TOTPSecret)
	var buf [8]byte
	binary.BigEndian.PutUint64(buf[:], uint64(at.Unix()/30))
	h := hmac.New(sha1.New, key)
	h.Write(buf[:])
	b := h.Sum(nil)
	o := b[len(b)-1] & 15
	return fmt.Sprintf("%06d", (binary.BigEndian.Uint32(b[o:o+4])&0x7fffffff)%1000000)
}
func testServer(t *testing.T) *Server {
	t.Helper()
	dir := t.TempDir()
	c, err := NewCredentials("owner", testPassword)
	if err != nil {
		t.Fatal(err)
	}
	if err = WriteCredentials(filepath.Join(dir, "auth.json"), c); err != nil {
		t.Fatal(err)
	}
	if err = config.AtomicWrite(filepath.Join(dir, "token"), []byte(RandomToken()), 0600); err != nil {
		t.Fatal(err)
	}
	s, err := NewServer("https://hmux.example", filepath.Join(dir, "auth.json"), filepath.Join(dir, "token"), fstest.MapFS{"index.html": {Data: []byte("HMux")}})
	if err != nil {
		t.Fatal(err)
	}
	s.locations = nil // Authentication tests never call an external location service.
	t.Cleanup(s.Close)
	return s
}
func request(s *Server, method, path string, body any, token, csrf, origin string) *httptest.ResponseRecorder {
	var b []byte
	if body != nil {
		b, _ = json.Marshal(body)
	}
	r := httptest.NewRequest(method, s.origin+path, bytes.NewReader(b))
	if body != nil {
		r.Header.Set("Content-Type", "application/json")
	}
	if token != "" {
		r.AddCookie(&http.Cookie{Name: cookieName, Value: token})
	}
	r.Header.Set("Origin", origin)
	r.Header.Set("X-CSRF-Token", csrf)
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	return w
}
func loginForTest(t *testing.T, s *Server) string {
	t.Helper()
	c := s.auth.credentials
	token, ok := s.auth.login(c.Username, testPassword, codeAt(c, time.Now()), time.Now())
	if !ok {
		t.Fatal("login failed")
	}
	return token
}
func TestAuthenticationBoundary(t *testing.T) {
	s := testServer(t)
	for _, path := range []string{"/api/state", "/api/session", "/api/terminal", "/api/action"} {
		if got := request(s, "GET", path, nil, "", "", "").Code; got != 401 {
			t.Fatalf("%s: %d", path, got)
		}
	}
	c := s.auth.credentials
	body := map[string]string{"username": "owner", "password": testPassword, "code": codeAt(c, time.Now())}
	if got := request(s, "POST", "/api/login", body, "", "", "https://evil.example").Code; got != 403 {
		t.Fatal(got)
	}
	body["password"] = "wrong"
	if got := request(s, "POST", "/api/login", body, "", "", s.origin).Code; got != 401 {
		t.Fatal(got)
	}
	body["password"] = testPassword
	w := request(s, "POST", "/api/login", body, "", "", s.origin)
	if w.Code != 200 {
		t.Fatal(w.Code, w.Body.String())
	}
	cookies := w.Result().Cookies()
	if len(cookies) != 1 {
		t.Fatal("cookie missing")
	}
	cookie := cookies[0]
	if cookie.Name != cookieName || !cookie.Secure || !cookie.HttpOnly || cookie.SameSite != http.SameSiteStrictMode || cookie.Path != "/" || cookie.Domain != "" {
		t.Fatal("unsafe cookie")
	}
	token := cookie.Value
	csrf, done, ok := s.auth.get(token, false)
	if !ok {
		t.Fatal("session missing")
	}
	if got := request(s, "POST", "/api/logout", struct{}{}, token, "", s.origin).Code; got != 403 {
		t.Fatal("CSRF accepted", got)
	}
	if got := request(s, "POST", "/api/action", Message{Operation: "exec"}, token, csrf, s.origin).Code; got != 400 {
		t.Fatal("arbitrary operation accepted", got)
	}
	if got := request(s, "POST", "/api/login", body, "", "", s.origin).Code; got != 401 {
		t.Fatal("TOTP replay accepted", got)
	}
	if got := request(s, "POST", "/api/logout", struct{}{}, token, csrf, s.origin).Code; got != 200 {
		t.Fatal(got)
	}
	select {
	case <-done:
	default:
		t.Fatal("logout did not revoke open connections")
	}
	if got := request(s, "GET", "/api/state", nil, token, "", "").Code; got != 401 {
		t.Fatal("revoked session accepted", got)
	}
}
func TestLoginRateLimitAndPersistedReplay(t *testing.T) {
	s := testServer(t)
	c := s.auth.credentials
	now := time.Now()
	code := codeAt(c, now)
	for i := 0; i < 5; i++ {
		if _, ok := s.auth.login("owner", "wrong", code, now); ok {
			t.Fatal("wrong password accepted")
		}
	}
	if _, ok := s.auth.login("owner", testPassword, code, now); ok {
		t.Fatal("rate limit bypass")
	}
	later := now.Add(2 * time.Minute)
	token, ok := s.auth.login("owner", testPassword, codeAt(c, later), later)
	if !ok || token == "" {
		t.Fatal("limit failed to recover")
	}
	restarted, err := newAuth(s.auth.path)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := restarted.login("owner", testPassword, codeAt(c, later), later); ok {
		t.Fatal("replay accepted after restart")
	}
}
func TestExpiredSessionsRevokeOnAbsoluteDeadline(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	_, done, _ := s.auth.get(token, false)
	s.auth.mu.Lock()
	s.auth.sessions[sessionKey(token)].expires = time.Now().Add(-time.Second)
	s.auth.mu.Unlock()
	if _, _, ok := s.auth.get(token, false); ok {
		t.Fatal("expired session alive")
	}
	select {
	case <-done:
	default:
		t.Fatal("expired socket not notified")
	}
}
func TestSecretFilesAndOriginsFailClosed(t *testing.T) {
	s := testServer(t)
	if err := os.Chmod(s.auth.path, 0644); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadCredentials(s.auth.path); err == nil {
		t.Fatal("public credentials accepted")
	}
	if err := os.Chmod(s.auth.path, 0600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(t.TempDir(), "link")
	if err := os.Symlink(s.auth.path, link); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadCredentials(link); err == nil {
		t.Fatal("symlink accepted")
	}
	for _, address := range []string{"0.0.0.0:8088", ":8088", "localhost:8088", "[::]:8088"} {
		if LoopbackAddress(address) == nil {
			t.Fatal("public listener accepted", address)
		}
	}
	r := httptest.NewRequest("GET", "https://evil.example/", nil)
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	if w.Code != 421 {
		t.Fatal("wrong host accepted")
	}
	for _, auth := range []string{"", "Bearer wrong"} {
		r := httptest.NewRequest("GET", s.origin+"/connect", nil)
		r.Header.Set("Authorization", auth)
		w := httptest.NewRecorder()
		s.ServeHTTP(w, r)
		if w.Code != 403 {
			t.Fatal("connector auth bypass")
		}
	}
}
func TestRFC6238SHA1Vector(t *testing.T) {
	c := Credentials{TOTPSecret: "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"}
	if got := c.MatchCode("287082", time.Unix(59, 0)); got != 1 {
		t.Fatal(got)
	}
	if c.MatchCode("287082", time.Unix(120, 0)) != -1 {
		t.Fatal("stale code accepted")
	}
}

func TestWebSocketOriginAndLogout(t *testing.T) {
	if os.Getenv("HMUX_RUN_WEB_SOCKET_TEST") != "1" {
		t.Skip("opt-in isolated loopback sockets; no tmux")
	}
	s := testServer(t)
	ts := httptest.NewServer(s)
	defer ts.Close()
	s.origin = ts.URL
	s.host = strings.TrimPrefix(ts.URL, "http://")
	token := loginForTest(t, s)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	url := "ws" + strings.TrimPrefix(ts.URL, "http")
	headers := http.Header{"Cookie": {cookieName + "=" + token}, "Origin": {"https://evil.example"}}
	if c, res, err := websocket.Dial(ctx, url+"/api/terminal", &websocket.DialOptions{HTTPHeader: headers}); err == nil {
		c.CloseNow()
		t.Fatal("cross-origin socket accepted")
	} else if res.StatusCode != 403 {
		t.Fatal(res.StatusCode)
	}
	home, _, err := websocket.Dial(ctx, url+"/connect", &websocket.DialOptions{HTTPHeader: http.Header{"Authorization": {"Bearer " + s.token}}})
	if err != nil {
		t.Fatal(err)
	}
	defer home.CloseNow()
	hp := &peer{conn: home}
	forwarded := make(chan Message, 2)
	// Fake Home replies only; this test never starts or attaches tmux.
	go func() {
		for {
			m, err := hp.read(ctx)
			if err != nil {
				return
			}
			if m.Type == "open" {
				_ = hp.send(ctx, Message{Type: "response", ID: m.ID, Payload: json.RawMessage(`{"ok":true}`)})
			}
			if m.Type == "open" || m.Type == "refresh" {
				forwarded <- m
			}
			if m.Type == "refresh" {
				_ = hp.send(ctx, Message{Type: "refresh-result", ID: m.ID, Error: "private detail must not reach browser"})
			}
		}
	}()
	headers.Set("Origin", s.origin)
	terminal, _, err := websocket.Dial(ctx, url+"/api/terminal", &websocket.DialOptions{HTTPHeader: headers})
	if err != nil {
		t.Fatal(err)
	}
	defer terminal.CloseNow()
	err = terminal.Write(ctx, websocket.MessageText, []byte(`{"type":"open","session":{"id":"$1","created_at":42},"cols":80,"rows":24}`))
	if err != nil {
		t.Fatal(err)
	}
	_, raw, err := terminal.Read(ctx)
	if err != nil || string(raw) != `{"type":"ready","heartbeat":true}` {
		t.Fatalf("not ready: %s %v", raw, err)
	}

	opened := <-forwarded
	if err = terminal.Write(ctx, websocket.MessageText, []byte(`{"type":"refresh","id":"foreign","session":{"id":"$99","created_at":1},"data":"a2lsbA=="}`)); err != nil {
		t.Fatal(err)
	}
	select {
	case frame := <-forwarded:
		if frame.Type != "refresh" || frame.ID != opened.ID || len(frame.Data) != 0 || frame.Session.ID != "" {
			t.Fatalf("unsafe refresh forwarding: %+v", frame)
		}
	case <-ctx.Done():
		t.Fatal("refresh not forwarded")
	}

	kind, result, err := terminal.Read(ctx)
	if err != nil || kind != websocket.MessageText || string(result) != `{"type":"refresh-result","ok":false}` {
		t.Fatalf("refresh result: %s %v", result, err)
	}
	// An idle terminal still exposes transport health to browser JavaScript.
	// No real Home, tmux session or application input is involved.
	kind, result, err = terminal.Read(ctx)
	if err != nil || kind != websocket.MessageText || string(result) != `{"type":"heartbeat"}` {
		t.Fatalf("heartbeat: %s %v", result, err)
	}
	s.auth.logout(token)
	if _, _, err = terminal.Read(ctx); err == nil {
		t.Fatal("socket survived logout")
	}
}

// TestBrowserPreview is a bounded fake-Home UI fixture, never a production mode.
func TestBrowserPreview(t *testing.T) {
	if os.Getenv("HMUX_WEB_BROWSER_PREVIEW") != "1" {
		t.Skip("manual browser fixture")
	}
	s := testServer(t)
	s.assets = http.FileServer(http.Dir("../../web/dist"))
	s.auth.credentials.TOTPSecret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
	if err := WriteCredentials(s.auth.path, s.auth.credentials); err != nil {
		t.Fatal(err)
	}
	ts := httptest.NewTLSServer(s)
	defer ts.Close()
	s.origin = ts.URL
	s.host = strings.TrimPrefix(ts.URL, "https://")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	home, _, err := websocket.Dial(ctx, "wss"+strings.TrimPrefix(ts.URL, "https")+"/connect", &websocket.DialOptions{HTTPClient: ts.Client(), HTTPHeader: http.Header{"Authorization": {"Bearer " + s.token}}})
	if err != nil {
		t.Fatal(err)
	}
	defer home.CloseNow()
	hp := &peer{conn: home}
	fixture := json.RawMessage(`{"sessions":[{"id":"$1","created_at":42,"name":"workspace","alias":"HMux web","runtime":"codex","window_count":2,"attached_clients":1},{"id":"$2","created_at":43,"name":"notes","alias":"Writing","runtime":"claude","window_count":1,"attached_clients":0}]}`)
	go func() {
		for {
			m, err := hp.read(ctx)
			if err != nil {
				return
			}
			switch m.Type {
			case "open":
				_ = hp.send(ctx, Message{Type: "response", ID: m.ID, Payload: json.RawMessage(`{"ok":true}`)})
				_ = hp.send(ctx, Message{Type: "data", ID: m.ID, Data: []byte("\x1b[32m❯\x1b[0m HMux web terminal\r\n한글 · tmux · Home workspace\r\n$ ")})
			case "input":
				_ = hp.send(ctx, Message{Type: "data", ID: m.ID, Data: m.Data})
			case "request":
				payload := json.RawMessage(`{"ok":true}`)
				if m.Operation == "workspace" {
					payload = json.RawMessage(`{"version":1,"initialized":true,"revision":1,"tabs":[{"id":"$1","created_at":42},{"id":"$2","created_at":43}]}`)
				}
				if m.Operation == "conversation" {
					payload = json.RawMessage(`{"status":"ready","messages":[{"role":"assistant","text":"현재 tmux 세션의 대화입니다.\n한글 입력과 대화 읽기를 확인합니다."}],"truncated":false}`)
				}
				_ = hp.send(ctx, Message{Type: "response", ID: m.ID, Payload: payload})
			}
		}
	}()
	go func() {
		tick := time.NewTicker(3 * time.Second)
		defer tick.Stop()
		for {
			_ = hp.send(ctx, Message{Type: "catalog", Payload: fixture})
			select {
			case <-ctx.Done():
				return
			case <-tick.C:
			}
		}
	}()
	if path := os.Getenv("HMUX_WEB_PREVIEW_URL_FILE"); path != "" {
		if err := os.WriteFile(path, []byte(ts.URL), 0600); err != nil {
			t.Fatal(err)
		}
	}
	fmt.Fprintln(os.Stderr, "Browser fixture ready", ts.URL)
	select {
	case <-time.After(10 * time.Minute):
	}
}

func TestLoginRateLimitIsolatedBySource(t *testing.T) {
	s := testServer(t)
	now := time.Now()
	code := codeAt(s.auth.credentials, now)
	for i := 0; i < 5; i++ {
		s.auth.login("owner", "wrong", code, now, "198.51.100.1")
	}
	if _, ok := s.auth.login("owner", testPassword, code, now, "198.51.100.2"); !ok {
		t.Fatal("one source locked out another")
	}
	r := httptest.NewRequest("GET", "https://hmux.example/", nil)
	r.RemoteAddr = "198.51.100.3:1234"
	r.Header.Set("X-Real-IP", "198.51.100.4")
	if loginSource(r) != "198.51.100.3" {
		t.Fatal("untrusted forwarding header accepted")
	}
	r.RemoteAddr = "127.0.0.1:1234"
	if loginSource(r) != "198.51.100.4" {
		t.Fatal("local proxy address ignored")
	}
}

func TestSharedWorkspacePollingDoesNotKeepLoginAlive(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	before := time.Now().Add(-29 * time.Minute)
	s.auth.mu.Lock()
	s.auth.sessions[sessionKey(token)].seen = before
	s.auth.mu.Unlock()
	// Home is deliberately offline: forwarding fails promptly, but an automatic
	// authenticated workspace read must not count as owner activity.
	body := map[string]any{"operation": "workspace", "payload": map[string]any{"change": nil}}
	if got := request(s, "POST", "/api/action", body, token, csrf, s.origin).Code; got != 502 {
		t.Fatal(got)
	}
	s.auth.mu.Lock()
	seen := s.auth.sessions[sessionKey(token)].seen
	s.auth.mu.Unlock()
	if !seen.Equal(before) {
		t.Fatal("workspace polling extended idle login")
	}
	body["payload"] = map[string]any{"change": map[string]any{"operation_id": "operation-00000001", "revision": 0, "base": []any{}, "tabs": []any{}}}
	_ = request(s, "POST", "/api/action", body, token, csrf, s.origin)
	s.auth.mu.Lock()
	seen = s.auth.sessions[sessionKey(token)].seen
	s.auth.mu.Unlock()
	if !seen.After(before) {
		t.Fatal("explicit workspace edit did not count as activity")
	}
}

func TestPWAContentSecurityPolicy(t *testing.T) {
	s := testServer(t)
	s.assets = http.FileServer(http.FS(fstest.MapFS{
		"index.html":    {Data: []byte("HMux")},
		"manifest.json": {Data: []byte(`{"name":"HMux"}`)},
		"sw.js":         {Data: []byte("// network-only worker")},
	}))
	for _, path := range []string{"/", "/manifest.json", "/sw.js"} {
		r := request(s, "GET", path, nil, "", "", "")
		if r.Code != http.StatusOK {
			t.Fatalf("%s: %d", path, r.Code)
		}
		directives := map[string]string{}
		for _, directive := range strings.Split(r.Header().Get("Content-Security-Policy"), ";") {
			parts := strings.Fields(directive)
			if len(parts) > 1 {
				directives[parts[0]] = strings.Join(parts[1:], " ")
			}
		}
		for _, name := range []string{"manifest-src", "worker-src", "script-src"} {
			if directives[name] != "'self'" {
				t.Errorf("%s: %s must permit only same-origin resources", path, name)
			}
		}
		if directives["default-src"] != "'none'" {
			t.Error("default resource restriction weakened")
		}
	}
}

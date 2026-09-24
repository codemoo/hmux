package webgateway

// Opt-in subprocess test of native gateway/Home pairs. All Home paths and
// process tools are synthetic; no existing tmux/provider state is read.
import (
	"bufio"
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/coder/websocket"
	"golang.org/x/sys/unix"
)

const fullGatewayToken = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

func TestRustFullGatewayWithGoHome(t *testing.T) {
	soakDuration := fullGatewaySoakDuration(t)
	perfCount := 0
	if value := os.Getenv("HMUX_NATIVE_PERF_COUNT"); value != "" {
		var err error
		perfCount, err = strconv.Atoi(value)
		if err != nil || perfCount < 100 || perfCount > 500000 {
			t.Fatal("HMUX_NATIVE_PERF_COUNT must be 100..500000")
		}
	}
	perfBytes := 64
	perfWarmup := 20
	perfIdle := 2
	for _, option := range []struct {
		name  string
		value *int
		min   int
		max   int
	}{{"HMUX_NATIVE_PERF_BYTES", &perfBytes, 64, 4096}, {"HMUX_NATIVE_PERF_WARMUP", &perfWarmup, 1, 100}, {"HMUX_NATIVE_PERF_IDLE_SECONDS", &perfIdle, 1, 120}} {
		if value := os.Getenv(option.name); value != "" {
			parsed, err := strconv.Atoi(value)
			if err != nil || parsed < option.min || parsed > option.max {
				t.Fatalf("%s must be %d..%d", option.name, option.min, option.max)
			}
			*option.value = parsed
		}
	}
	churn := 0
	if value := os.Getenv("HMUX_NATIVE_CHURN"); value != "" {
		var err error
		churn, err = strconv.Atoi(value)
		if err != nil || churn < 1 || churn > 10000 {
			t.Fatal("HMUX_NATIVE_CHURN must be 1..10000")
		}
	}
	capacity := os.Getenv("HMUX_NATIVE_CAPACITY") == "1"
	activity := os.Getenv("HMUX_NATIVE_ACTIVITY") == "1"
	if value := os.Getenv("HMUX_NATIVE_ACTIVITY"); value != "" && value != "1" {
		t.Fatal("HMUX_NATIVE_ACTIVITY must be 1 when set")
	}
	if value := os.Getenv("HMUX_NATIVE_CAPACITY"); value != "" && value != "1" {
		t.Fatal("HMUX_NATIVE_CAPACITY must be 1 when set")
	}
	if capacity && (churn > 0 || perfCount > 0 || soakDuration > 0) {
		t.Fatal("capacity cannot be combined with perf/churn/soak")
	}
	if activity && (capacity || churn > 0 || perfCount > 0 || soakDuration > 0) {
		t.Fatal("activity cannot be combined with capacity/perf/churn/soak")
	}
	implementation := os.Getenv("HMUX_GATEWAY_IMPLEMENTATION")
	if implementation != "" && implementation != "rust" && implementation != "go" {
		t.Fatal("HMUX_GATEWAY_IMPLEMENTATION must be go or rust")
	}
	binary := os.Getenv("HMUX_RUST_GATEWAY_BIN")
	if implementation == "go" {
		binary = os.Getenv("HMUX_GO_GATEWAY_BIN")
	}
	if binary == "" {
		t.Skip("set an absolute HMUX_RUST_GATEWAY_BIN (or HMUX_GO_GATEWAY_BIN for Go serve)")
	}
	if !filepath.IsAbs(binary) {
		t.Fatal("absolute candidate binary path required")
	}
	nativeHome := os.Getenv("HMUX_NATIVE_HOME_BIN")
	if soakDuration > 0 && (nativeHome == "" || churn > 0 || perfCount > 0) {
		t.Fatal("soak requires a native Home and cannot be combined with perf/churn")
	}
	if nativeHome != "" && !filepath.IsAbs(nativeHome) {
		t.Fatal("absolute HMUX_NATIVE_HOME_BIN required")
	}
	homeImplementation := os.Getenv("HMUX_NATIVE_HOME_IMPLEMENTATION")
	if homeImplementation != "" && homeImplementation != "rust" && homeImplementation != "go" {
		t.Fatal("HMUX_NATIVE_HOME_IMPLEMENTATION must be go or rust")
	}
	if capacity && (implementation == "go" || nativeHome == "" || homeImplementation == "go") {
		t.Fatal("capacity requires native Rust gateway and Home")
	}
	if activity && (implementation == "go" || nativeHome == "" || homeImplementation == "go") {
		t.Fatal("activity requires native Rust gateway and Home")
	}
	goHomeCore := nativeHome == "" && homeImplementation == "go"
	jsonFallback := os.Getenv("HMUX_NATIVE_JSON_FALLBACK") == "1"
	if os.Getenv("HMUX_NATIVE_JSON_FALLBACK") != "" && !jsonFallback {
		t.Fatal("HMUX_NATIVE_JSON_FALLBACK must be 1 when set")
	}
	if jsonFallback && (nativeHome == "" || homeImplementation == "go") {
		t.Fatal("JSON fallback requires a native Rust Home")
	}
	root, err := os.MkdirTemp("", "hmux-e2e-go-home-")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(root)
	root, err = filepath.EvalSymlinks(root)
	if err != nil {
		t.Fatal(err)
	}
	write := func(name, data string, mode os.FileMode) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(root, name), []byte(data), mode); err != nil {
			t.Fatal(err)
		}
	}
	for _, name := range []string{"assets", "home", "state", "bin", "views", "view-owners", "view-hooks"} {
		if err := os.Mkdir(filepath.Join(root, name), 0700); err != nil {
			t.Fatal(err)
		}
	}
	var activityFiles fullGatewayActivityFiles
	if activity {
		activityFiles = fullGatewayActivitySeed(t, filepath.Join(root, "home"))
	}
	credentials, err := NewCredentials("synthetic", "synthetic-go-home-password")
	if err != nil {
		t.Fatal(err)
	}
	credentials.TOTPDisabled = true
	if err := WriteCredentials(filepath.Join(root, "credentials.json"), credentials); err != nil {
		t.Fatal(err)
	}
	write("connector.token", fullGatewayToken, 0600)
	write("assets/index.html", "<!doctype html><title>Synthetic HMux</title>", 0600)
	write("inventory.toml", fmt.Sprintf("schema_version = 1\nrevision = 'synthetic'\n[[profiles]]\nid = 'shell'\nlabel = 'Synthetic shell'\ndefault_directory = %q\ncommand = ['/bin/sh']\n", filepath.Join(root, "home")), 0600)
	testBinary, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"tmux", "ps", "lsof"} {
		write("bin/"+name, "#!/bin/sh\nunset GOGC GOMEMLIMIT GOMAXPROCS GODEBUG\nexport HMUX_GO_E2E_TOOL="+name+"\nexec \"$HMUX_GO_E2E_BINARY\" -test.run '^TestRustFullGatewayTool$' -- \"$@\"\n", 0700)
	}
	// No inherited credentials, provider endpoints, tmux socket or application paths.
	env := []string{"HOME=" + filepath.Join(root, "home"), "PATH=" + filepath.Join(root, "bin") + ":/usr/bin:/bin", "TMPDIR=" + root, "LANG=C", "TZ=UTC", "HMUX_GO_E2E_ROOT=" + root, "HMUX_GO_E2E_BINARY=" + testBinary, "TOKEN_USAGE_DISABLE_CLAUDE_SWAP=1", "TOKEN_USAGE_DISABLE_CODEX_ACCOUNTS=1", "GORACE=atexit_sleep_ms=0"}
	if activity {
		env = append(env, "TOKEN_USAGE_CLAUDE_PROJECTS="+activityFiles.claudeRoot,
			"TOKEN_USAGE_CODEX_SESSIONS="+activityFiles.codexRoot)
	} else {
		env = append(env, "TOKEN_USAGE_DISABLE_JSONL=1")
	}
	if soakDuration > 0 {
		env = append(env, "HMUX_GO_E2E_SOAK=1")
	}
	for _, name := range []string{"GOGC", "GOMEMLIMIT"} {
		if perfCount > 0 && os.Getenv(name) != "" {
			t.Fatalf("performance oracle must not inherit %s; use HMUX_PERF_GO_%s", name, name)
		}
		if value, ok := os.LookupEnv("HMUX_PERF_GO_" + name); perfCount > 0 && ok {
			env = append(env, name+"="+value)
		}
	}
	// A synthetic CA makes this protocol test independent of host keychain access.
	// Certificate validation remains enabled; native OS trust has its own test.
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	cert := &x509.Certificate{SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "HMux synthetic test root"}, NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(soakDuration + time.Hour), IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageCertSign | x509.KeyUsageCRLSign}
	der, err := x509.CreateCertificate(rand.Reader, cert, cert, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	write("root.pem", string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})), 0600)
	env = append(env, "SSL_CERT_FILE="+filepath.Join(root, "root.pem"))
	write("home.toml", fmt.Sprintf("schema_version = 1\nrole = 'home'\ninventory_path = %q\nstate_dir = %q\n", filepath.Join(root, "inventory.toml"), filepath.Join(root, "state")), 0600)
	entrypoint := "--experimental-gateway"
	productionCLI := os.Getenv("HMUX_RUST_GATEWAY_PRODUCTION") != "" || implementation == "go"
	if productionCLI {
		entrypoint = "serve"
	}
	listen := "127.0.0.1:0"
	if implementation == "go" {
		// Go's serve CLI reports its configured address before ListenAndServe.
		port, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		listen = port.Addr().String()
		if err := port.Close(); err != nil {
			t.Fatal(err)
		}
	}
	gateway := exec.Command(binary, entrypoint, "--origin", "https://hmux.example", "--credentials", filepath.Join(root, "credentials.json"), "--token-file", filepath.Join(root, "connector.token"), "--assets", filepath.Join(root, "assets"), "--listen", listen)
	gateway.Env = env
	if implementation != "go" {
		assertRustGatewayStartupSignal(t, gateway.Args, env, root)
	}
	var readiness io.ReadCloser
	if productionCLI {
		readiness, err = gateway.StdoutPipe()
	} else {
		readiness, err = gateway.StderrPipe()
	}
	if err != nil {
		t.Fatal(err)
	}
	if err := gateway.Start(); err != nil {
		t.Fatal(err)
	}
	gatewayDone := make(chan error, 1)
	go func() { gatewayDone <- gateway.Wait() }()
	gatewayStopped := false
	defer func() {
		if !gatewayStopped {
			_ = gateway.Process.Kill()
			<-gatewayDone
		}
	}()
	ready := make(chan string, 1)
	go func() {
		reader := bufio.NewReader(readiness)
		line, _ := reader.ReadString('\n')
		ready <- line
		_, _ = io.Copy(io.Discard, reader)
	}()
	var address string
	select {
	case line := <-ready:
		prefix := "Experimental gateway listener ready at "
		if productionCLI {
			prefix = "HMux web listening on "
		}
		if !strings.HasPrefix(line, prefix) {
			t.Fatalf("candidate startup failed: %s", line)
		}
		address = strings.TrimSpace(strings.TrimPrefix(line, prefix))
		if productionCLI {
			address = strings.TrimSuffix(address, " behind HTTPS")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("candidate readiness timeout")
	}
	host, port, err := net.SplitHostPort(address)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() || port == "0" {
		t.Fatalf("gateway reported a non-loopback listener: %q", address)
	}
	if implementation == "go" {
		// Its readiness line precedes bind; wait before exercising HTTP or WSS.
		deadline := time.Now().Add(10 * time.Second)
		for {
			conn, dialErr := net.DialTimeout("tcp", address, 100*time.Millisecond)
			if dialErr == nil {
				_ = conn.Close()
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("Go gateway did not bind: %v", dialErr)
			}
			time.Sleep(20 * time.Millisecond)
		}
	}
	proxyEndpoint := ""
	var proxyNegotiations func() []fullGatewayNegotiation
	if nativeHome != "" || goHomeCore {
		proxyEndpoint, proxyNegotiations = fullGatewayTLSProxy(t, address, cert, key, jsonFallback)
	}
	client := &http.Client{Transport: fullGatewayTransport(address), Timeout: 30 * time.Second}
	defer client.CloseIdleConnections()
	cookie, csrf := "", ""
	request := func(method, path string, body any) (int, http.Header, []byte) {
		t.Helper()
		var input io.Reader
		if body != nil {
			raw, err := json.Marshal(body)
			if err != nil {
				t.Fatal(err)
			}
			input = bytes.NewReader(raw)
		}
		req, err := http.NewRequest(method, "http://hmux.example"+path, input)
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("Origin", "https://hmux.example")
		req.Header.Set("Cookie", cookie)
		req.Header.Set("X-CSRF-Token", csrf)
		req.Header.Set("Content-Type", "application/json")
		res, err := client.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		defer res.Body.Close()
		raw, err := io.ReadAll(io.LimitReader(res.Body, 2<<20))
		if err != nil {
			t.Fatal(err)
		}
		return res.StatusCode, res.Header, raw
	}
	status, _, _ := request("GET", "/api/state", nil)
	if status != 401 {
		t.Fatalf("anonymous state: %d", status)
	}
	status, _, raw := request("GET", "/", nil)
	if status != 200 || !bytes.Contains(raw, []byte("Synthetic HMux")) {
		t.Fatal("assembled static assets unavailable")
	}
	status, headers, _ := request("POST", "/api/login", map[string]string{"username": "synthetic", "password": "synthetic-go-home-password"})
	if status != 200 {
		t.Fatalf("login: %d", status)
	}
	cookie = strings.Split(headers.Get("Set-Cookie"), ";")[0]
	status, _, raw = request("GET", "/api/session", nil)
	var session struct {
		CSRF string `json:"csrf"`
	}
	if status != 200 || json.Unmarshal(raw, &session) != nil || session.CSRF == "" {
		t.Fatal("session unavailable")
	}
	csrf = session.CSRF
	var home *exec.Cmd
	var homeDone chan error
	var homeLog fullGatewayBoundedLog
	startHome := func() {
		t.Helper()
		homeLog.Reset()
		if nativeHome == "" {
			home = exec.Command(testBinary, "-test.run", "^TestRustFullGatewayGoHome$")
			home.Env = append(append([]string{}, env...), "HMUX_GO_E2E_ADDRESS="+address, "HMUX_GO_E2E_WSS="+proxyEndpoint)
		} else {
			home = exec.Command(nativeHome, "connect", "--url", proxyEndpoint, "--token-file", filepath.Join(root, "connector.token"), "--config", filepath.Join(root, "home.toml"))
			home.Env = append([]string{}, env...)
		}
		home.WaitDelay = 3 * time.Second
		home.Stdout = &homeLog
		home.Stderr = &homeLog
		if err := home.Start(); err != nil {
			t.Fatal(err)
		}
		homeDone = make(chan error, 1)
		command := home
		done := homeDone
		go func() { done <- command.Wait() }()
	}
	stopHome := func() {
		t.Helper()
		if home == nil {
			return
		}
		_ = home.Process.Signal(syscall.SIGTERM)
		select {
		case err := <-homeDone:
			home = nil
			if err != nil {
				t.Fatalf("Home stop: %v: %s", err, homeLog.String())
			}
		case <-time.After(15 * time.Second):
			_ = home.Process.Kill()
			<-homeDone
			home = nil
			t.Fatal("Home workers failed to join")
		}
		home = nil
	}
	defer func() {
		if home != nil {
			_ = home.Process.Kill()
			<-homeDone
			if t.Failed() {
				t.Logf("synthetic Home: %s", homeLog.String())
			}
		}
	}()
	wait := func(label string, condition func() bool) {
		t.Helper()
		end := time.Now().Add(15 * time.Second)
		for time.Now().Before(end) {
			if home != nil {
				select {
				case err := <-homeDone:
					home = nil
					t.Fatalf("Home ended while waiting for %s: %v: %s", label, err, homeLog.String())
				default:
				}
			}
			if condition() {
				return
			}
			time.Sleep(50 * time.Millisecond)
		}
		t.Fatal(label)
	}
	var lastState []byte
	defer func() {
		if t.Failed() {
			t.Logf("last synthetic state: %.2048s", lastState)
		}
	}()
	stateOnline := func(want bool) bool {
		status, _, raw := request("GET", "/api/state", nil)
		lastState = raw
		var state struct {
			Online  bool           `json:"online"`
			Catalog *model.Catalog `json:"catalog"`
		}
		if status != 200 || json.Unmarshal(raw, &state) != nil {
			return false
		}
		return state.Online == want && (!want || (state.Catalog != nil && len(state.Catalog.Sessions) == 1 && state.Catalog.Sessions[0].ID == "$7" && state.Catalog.Sessions[0].CreatedAt == 42))
	}
	noViews := func() bool {
		entries, err := os.ReadDir(filepath.Join(root, "views"))
		if err != nil {
			return false
		}
		for _, entry := range entries {
			// Emulate tmux's verified client-detached hook when its last client
			// has exited. A native Go main may finish before async cleanup; the
			// real tmux server still executes this independently of Home.
			path := filepath.Join(root, "views", entry.Name())
			raw, _ := os.ReadFile(path)
			pid, _ := strconv.Atoi(string(raw))
			_, hookErr := os.Stat(filepath.Join(root, "view-hooks", entry.Name()))
			if pid > 1 && hookErr == nil && errors.Is(syscall.Kill(pid, 0), syscall.ESRCH) {
				_ = os.Remove(path)
			}
		}
		entries, err = os.ReadDir(filepath.Join(root, "views"))
		return err == nil && len(entries) == 0
	}
	startHome()
	wait("Home catalog did not arrive", func() bool { return stateOnline(true) })
	if nativeHome != "" || goHomeCore {
		negotiations := proxyNegotiations()
		if len(negotiations) != 1 {
			t.Fatalf("expected one successful native Home upgrade, got %+v", negotiations)
		}
		got := negotiations[0]
		wantOffered := "hmux-home.pb.v2.controls1"
		if homeImplementation == "go" {
			wantOffered = ""
		}
		if got.Offered != wantOffered {
			t.Fatalf("native Home offered %q, want %q", got.Offered, wantOffered)
		}
		wantForwarded, wantSelected := wantOffered, wantOffered
		if jsonFallback || implementation == "go" {
			wantSelected = ""
		}
		if jsonFallback {
			wantForwarded = ""
		}
		if got.Forwarded != wantForwarded || got.Selected != wantSelected {
			t.Fatalf("Home WebSocket negotiation: got %+v, want forwarded=%q selected=%q", got, wantForwarded, wantSelected)
		}
	}
	wait("Home usage did not pass gateway allowlist", func() bool {
		status, _, raw := request("GET", "/api/state", nil)
		var state struct {
			Usage map[string]struct {
				Schema   int                        `json:"schema"`
				Provider string                     `json:"provider"`
				Sources  map[string]json.RawMessage `json:"sources"`
			} `json:"usage"`
		}
		if status != 200 || json.Unmarshal(raw, &state) != nil {
			return false
		}
		for provider, secondary := range map[string]string{"claude": "cswap", "codex": "codex-lb"} {
			item := state.Usage[provider]
			if item.Schema != 1 || item.Provider != provider || len(item.Sources["cli"]) == 0 || len(item.Sources[secondary]) == 0 {
				return false
			}
		}
		return true
	})
	status, _, raw = request("POST", "/api/action", map[string]any{"operation": "profiles"})
	if status != 200 || !bytes.Contains(raw, []byte("Synthetic shell")) {
		t.Fatalf("Home profiles: %d %s", status, raw)
	}
	status, _, raw = request("POST", "/api/action", map[string]any{"operation": "workspace", "payload": map[string]any{}})
	if status != 200 || !bytes.Contains(raw, []byte(`"version":1`)) {
		t.Fatalf("Home workspace: %d %s", status, raw)
	}
	perfAllowance := time.Duration(0)
	if perfCount > 0 {
		// Bound the internal deadline even at the maximum sustained workload.
		perfAllowance = time.Duration(perfIdle)*time.Second + time.Duration(perfCount)*2*time.Millisecond
	}
	activityAllowance := time.Duration(0)
	if activity {
		activityAllowance = 50 * time.Second
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute+time.Duration(churn)*5*time.Second+perfAllowance+activityAllowance+soakDuration)
	defer cancel()
	openIdentity := func(createdAt int) *websocket.Conn {
		t.Helper()
		conn, _, err := websocket.Dial(ctx, "ws://hmux.example/api/terminal", &websocket.DialOptions{HTTPClient: client, HTTPHeader: http.Header{"Origin": {"https://hmux.example"}, "Cookie": {cookie}}})
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = conn.CloseNow() })
		raw := []byte(fmt.Sprintf(`{"type":"open","session":{"id":"$7","created_at":%d},"cols":80,"rows":24,"capabilities":["terminal-output-flow-v1"]}`, createdAt))
		if err = conn.Write(ctx, websocket.MessageText, raw); err != nil {
			t.Fatal(err)
		}
		return conn
	}
	open := func() (*websocket.Conn, *int) {
		t.Helper()
		conn := openIdentity(42)
		kind, raw, err := conn.Read(ctx)
		if err != nil || kind != websocket.MessageText || !bytes.Contains(raw, []byte(`"output_flow":true`)) {
			t.Fatalf("terminal ready: %s %v", raw, err)
		}
		return conn, new(int)
	}
	readUntil := func(conn *websocket.Conn, received *int, expected string) []byte {
		t.Helper()
		readCtx, stop := context.WithTimeout(ctx, 10*time.Second)
		defer stop()
		var output []byte
		for !bytes.Contains(output, []byte(expected)) {
			kind, raw, err := conn.Read(readCtx)
			if err != nil {
				t.Fatalf("terminal output %q: %v (tail %q)", expected, err, output[max(0, len(output)-256):])
			}
			if kind != websocket.MessageBinary {
				continue
			}
			output = append(output, raw...)
			if len(output) > 2<<20 {
				t.Fatal("unexpected excessive output")
			}
			*received += len(raw)
			ack := fmt.Sprintf(`{"type":"output-ack","received":%d}`, len(raw))
			if err := conn.Write(readCtx, websocket.MessageText, []byte(ack)); err != nil {
				t.Fatal(err)
			}
		}
		return output
	}
	readClosed := func(conn *websocket.Conn, label string) error {
		t.Helper()
		readCtx, stop := context.WithTimeout(ctx, 5*time.Second)
		defer stop()
		// Output already queued before disconnect can precede the close frame.
		// Bound the drain and require actual closure, never a read timeout.
		bytesRead := 0
		for frames := 0; frames < 128; frames++ {
			_, raw, err := conn.Read(readCtx)
			if err != nil {
				if readCtx.Err() != nil {
					t.Fatalf("%s timed out waiting for closure: %v", label, err)
				}
				return err
			}
			bytesRead += len(raw)
			if bytesRead > 1<<20 {
				t.Fatalf("%s retained excessive output after disconnect", label)
			}
		}
		t.Fatalf("%s did not close within the frame bound", label)
		return nil
	}
	input := func(conn *websocket.Conn, text string) {
		t.Helper()
		if err := conn.Write(ctx, websocket.MessageBinary, []byte(text)); err != nil {
			t.Fatal(err)
		}
	}
	stale := openIdentity(43)
	staleCtx, staleStop := context.WithTimeout(ctx, 5*time.Second)
	_, _, err = stale.Read(staleCtx)
	staleStop()
	if websocket.CloseStatus(err) != websocket.StatusPolicyViolation {
		t.Fatalf("stale tmux lifetime accepted: %v", err)
	}
	wait("stale identity created a disposable view", noViews)
	browser, received := open()
	readUntil(browser, received, "HMUX-READY")
	if err := browser.Write(ctx, websocket.MessageText, []byte(`{"type":"resize","cols":100,"rows":30}`)); err != nil {
		t.Fatal(err)
	}
	input(browser, "size\n")
	readUntil(browser, received, "SIZE=100x30")
	input(browser, "burst\n")
	burst := readUntil(browser, received, "BURST-END")
	if bytes.Count(burst, []byte("x")) != 768<<10 {
		t.Fatal("burst output truncated")
	} // More than one full output window; every output frame must be ACKed.
	_ = browser.CloseNow()
	wait("closed browser retained disposable PTY", noViews)
	// Fill one browser's render-credit window without acknowledging it. A
	// second browser and control request must remain usable on the same Home.
	slow, slowReceived := open()
	readUntil(slow, slowReceived, "HMUX-READY")
	input(slow, "burst\n")
	held, heldXs := 0, 0
	var heldFrames []int
	for frames := 0; frames < terminalOutputFrames && held < terminalOutputBytes; frames++ {
		readCtx, stop := context.WithTimeout(ctx, 10*time.Second)
		kind, chunk, err := slow.Read(readCtx)
		stop()
		if err != nil || kind != websocket.MessageBinary || len(chunk) == 0 {
			t.Fatalf("slow view failed before filling its credit: %v", err)
		}
		held += len(chunk)
		heldXs += bytes.Count(chunk, []byte("x"))
		heldFrames = append(heldFrames, len(chunk))
	}
	if held > terminalOutputBytes {
		t.Fatal("slow view exceeded byte credit")
	}
	healthy, healthyReceived := open()
	readUntil(healthy, healthyReceived, "HMUX-READY")
	input(healthy, "burst\n")
	if bytes.Count(readUntil(healthy, healthyReceived, "BURST-END"), []byte("x")) != 768<<10 {
		t.Fatal("slow view blocked or corrupted healthy output")
	}
	status, _, raw = request("POST", "/api/action", map[string]any{"operation": "profiles"})
	if status != 200 || !bytes.Contains(raw, []byte("Synthetic shell")) {
		t.Fatalf("slow view blocked control request: %d %s", status, raw)
	}
	for _, size := range heldFrames {
		if err := slow.Write(ctx, websocket.MessageText, []byte(fmt.Sprintf(`{"type":"output-ack","received":%d}`, size))); err != nil {
			t.Fatal(err)
		}
	}
	if heldXs+bytes.Count(readUntil(slow, slowReceived, "BURST-END"), []byte("x")) != 768<<10 {
		t.Fatal("resumed slow view lost or duplicated bytes")
	}
	_ = healthy.CloseNow()
	_ = slow.CloseNow()
	wait("slow/healthy views retained disposable PTYs", noViews)
	if capacity {
		fullGatewayCapacity(t, fullGatewayCapacityFixture{
			root: root, gatewayPID: gateway.Process.Pid, homePID: home.Process.Pid,
			open: open, openIdentity: openIdentity, readUntil: readUntil, input: input,
			wait: wait, noViews: noViews, stateOnline: func() bool { return stateOnline(true) },
		})
	}
	if activity {
		fullGatewayActivity(t, fullGatewayActivityFixture{
			files: activityFiles, gatewayPID: gateway.Process.Pid, homePID: home.Process.Pid,
			open: open, readUntil: readUntil, input: input,
			requestState: func() (int, []byte) {
				status, _, raw := request("GET", "/api/state", nil)
				return status, raw
			},
			wait: wait, noViews: noViews,
		})
	}
	if perfCount > 0 {
		// One persistent view carries sequential fixed-size inputs. Timing ends at
		// the oracle's socket receipt, before any browser/xterm rendering.
		samplesPath := os.Getenv("HMUX_NATIVE_PERF_SAMPLES_PATH")
		if !filepath.IsAbs(samplesPath) {
			t.Fatal("absolute HMUX_NATIVE_PERF_SAMPLES_PATH required")
		}
		view, count := open()
		readUntil(view, count, "HMUX-READY")
		// coder/websocket handles ping while Read is active. Keep one reader
		// alive through connected idle and all echoes; only this goroutine reads.
		type perfFrame struct {
			kind websocket.MessageType
			raw  []byte
			err  error
		}
		frames := make(chan perfFrame, 2)
		readerCtx, stopReader := context.WithCancel(ctx)
		readerDone := make(chan struct{})
		go func() {
			defer close(readerDone)
			for {
				kind, raw, err := view.Read(readerCtx)
				select {
				case frames <- perfFrame{kind, raw, err}:
				case <-readerCtx.Done():
					return
				}
				if err != nil {
					return
				}
			}
		}()
		var closePerfOnce sync.Once
		closePerfView := func() {
			closePerfOnce.Do(func() {
				stopReader()
				_ = view.CloseNow()
				<-readerDone
			})
		}
		defer closePerfView()
		ackFrame := func(readCtx context.Context, frame perfFrame) []byte {
			t.Helper()
			if frame.err != nil {
				t.Fatalf("performance view read: %v", frame.err)
			}
			if frame.kind != websocket.MessageBinary {
				return nil
			}
			*count += len(frame.raw)
			ack := fmt.Sprintf(`{"type":"output-ack","received":%d}`, len(frame.raw))
			if err := view.Write(readCtx, websocket.MessageText, []byte(ack)); err != nil {
				t.Fatalf("performance view ACK: %v", err)
			}
			return frame.raw
		}
		readPerfUntil := func(expected string) {
			t.Helper()
			readCtx, stop := context.WithTimeout(ctx, 10*time.Second)
			defer stop()
			var output []byte
			for !bytes.Contains(output, []byte(expected)) {
				select {
				case frame := <-frames:
					output = append(output, ackFrame(readCtx, frame)...)
					if len(output) > 2<<20 {
						t.Fatal("unexpected excessive performance output")
					}
				case <-readCtx.Done():
					t.Fatalf("performance output %q: %v (tail %q)", expected, readCtx.Err(), output[max(0, len(output)-256):])
				}
			}
		}
		clockTicks := fullGatewayClockTicks(t)
		sample := func(stage string) {
			fullGatewayPerfResourceSample(t, stage, gateway.Process.Pid, home.Process.Pid, clockTicks)
		}
		echo := func(label string, sequence int) time.Duration {
			prefix := fmt.Sprintf("%s-%04d-", label, sequence)
			payload := prefix + strings.Repeat("x", perfBytes-len(prefix)-1)
			started := time.Now()
			input(view, payload+"\n")
			readPerfUntil("INPUT=" + payload)
			return time.Since(started)
		}
		sample("warmup-start")
		for iteration := 0; iteration < perfWarmup; iteration++ {
			echo("warm", iteration)
		}
		sample("warmup-end")
		sample("connected-idle-start")
		idleStarted := time.Now()
		for second := 0; second < perfIdle; second++ {
			timer := time.NewTimer(time.Second)
			idleBytes := 0
		idleSecond:
			for {
				select {
				case frame := <-frames:
					idleBytes += len(ackFrame(ctx, frame))
					if idleBytes > 2<<20 {
						t.Fatal("unexpected excessive connected-idle output")
					}
				case <-timer.C:
					break idleSecond
				case <-ctx.Done():
					t.Fatalf("connected idle: %v", ctx.Err())
				}
			}
			if (second+1)%max(1, (perfIdle+4)/5) == 0 || second+1 == perfIdle {
				sample(fmt.Sprintf("connected-idle-%d", second+1))
			}
		}
		t.Logf("native-perf-idle %s", fullGatewayPerfJSON(t, map[string]any{"seconds": time.Since(idleStarted).Seconds(), "samples": perfIdle, "cpu_resolution_seconds": 1 / clockTicks}))
		rtts := make([]float64, 0, perfCount)
		sample("echo-start")
		started := time.Now()
		for iteration := 0; iteration < perfCount; iteration++ {
			rtts = append(rtts, float64(echo("perf", iteration).Nanoseconds())/1e6)
			if (iteration+1)%max(1, (perfCount+9)/10) == 0 || iteration+1 == perfCount {
				sample(fmt.Sprintf("echo-%d", iteration+1))
			}
		}
		elapsed := time.Since(started).Seconds()
		// Preserve the raw samples once, outside go test -v output and its log.
		samplesFile, err := os.OpenFile(samplesPath, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			t.Fatal(err)
		}
		writeErr := json.NewEncoder(samplesFile).Encode(rtts)
		closeErr := samplesFile.Close()
		if writeErr != nil || closeErr != nil {
			t.Fatalf("write performance RTT samples: %v; close: %v", writeErr, closeErr)
		}
		slices.Sort(rtts)
		percentile := func(percent int) float64 {
			index := (len(rtts)*percent+99)/100 - 1
			return rtts[index]
		}
		t.Logf("native-perf-result %s", fullGatewayPerfJSON(t, map[string]any{
			"count": perfCount, "errors": 0, "input_bytes": perfBytes, "warmup_count": perfWarmup,
			"socket_receipt_samples_file": filepath.Base(samplesPath),
			"elapsed_seconds":             elapsed, "echoes_per_second": float64(perfCount) / elapsed,
			"socket_receipt_rtt_ms": map[string]float64{"p50": percentile(50), "p95": percentile(95), "p99": percentile(99)},
		}))
		closePerfView()
		wait("performance view retained disposable PTY", noViews)
	}
	if churn > 0 {
		started := time.Now()
		fullGatewayResourceSample(t, "before-churn", gateway.Process.Pid, home.Process.Pid)
		for iteration := 1; iteration <= churn; iteration++ {
			view, count := open()
			readUntil(view, count, "HMUX-READY")
			input(view, fmt.Sprintf("churn-%d\n", iteration))
			readUntil(view, count, fmt.Sprintf("INPUT=churn-%d", iteration))
			_ = view.CloseNow()
			wait("churn retained disposable PTY", noViews)
			if iteration%100 == 0 || iteration == churn {
				t.Logf("churn completed=%d elapsed=%s", iteration, time.Since(started))
				fullGatewayResourceSample(t, fmt.Sprintf("after-%d", iteration), gateway.Process.Pid, home.Process.Pid)
				if !stateOnline(true) {
					t.Fatal("churn lost shared Home connection")
				}
			}
		}
	}
	if soakDuration > 0 {
		// Reset after the bounded preconditions. Final lifecycle assertions get
		// a fresh deadline too; setup latency cannot steal time from the soak.
		cancel()
		ctx, cancel = context.WithTimeout(context.Background(), soakDuration+time.Minute)
		defer cancel()
		fullGatewaySoak(t, ctx, soakDuration, root, gateway.Process.Pid, home.Process.Pid,
			open, readUntil, input, func() bool { return stateOnline(true) }, wait, noViews)
		cancel()
		ctx, cancel = context.WithTimeout(context.Background(), time.Minute)
		defer cancel()
	}
	browser, received = open()
	readUntil(browser, received, "HMUX-READY")
	stopHome()
	wait("disconnected Home still online", func() bool { return stateOnline(false) })
	wait("Home shutdown retained disposable PTY", noViews)
	_ = readClosed(browser, "old browser generation")
	startHome()
	wait("replacement Home did not reconnect", func() bool { return stateOnline(true) })
	browser, received = open()
	readUntil(browser, received, "HMUX-READY")
	status, _, _ = request("POST", "/api/logout", nil)
	if status != 200 {
		t.Fatalf("logout: %d", status)
	}
	err = readClosed(browser, "revoked browser")
	// The Go gateway may close the socket during cancellation before its policy
	// close frame is written. Rust consistently sends 1008; keep that assertion.
	goRevokedEOF := implementation == "go" && errors.Is(err, io.EOF)
	if websocket.CloseStatus(err) != websocket.StatusPolicyViolation && !goRevokedEOF {
		t.Fatalf("revoked browser close: %v", err)
	}
	if status, _, _ := request("GET", "/api/state", nil); status != http.StatusUnauthorized {
		t.Fatalf("revoked session retained HTTP access: %d", status)
	}
	wait("revocation retained disposable PTY", noViews)
	stopHome()
	_ = gateway.Process.Signal(syscall.SIGTERM)
	select {
	case err := <-gatewayDone:
		gatewayStopped = true
		if err != nil {
			t.Fatalf("gateway SIGTERM: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("gateway shutdown did not join owners")
	}
	log, err := os.ReadFile(filepath.Join(root, "commands.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	for _, line := range bytes.Split(bytes.TrimSpace(log), []byte("\n")) {
		var args []string
		if json.Unmarshal(line, &args) != nil || len(args) == 0 {
			t.Fatal("bad synthetic command log")
		}
		if args[0] == "kill-session" && (len(args) != 3 || !strings.HasPrefix(args[2], "hmux-app-view-")) {
			t.Fatalf("original session mutation: %q", args)
		}
	}
	if nativeHome != "" || goHomeCore {
		negotiations := proxyNegotiations()
		if len(negotiations) != 2 || negotiations[0] != negotiations[1] {
			t.Fatalf("reconnected Home changed negotiated protocol: %+v", negotiations)
		}
	}
	t.Log("Home catalog, profiles, workspace, PTY ACK/input/resize, close/reconnect/logout and gateway SIGTERM passed")
}

type fullGatewayNegotiation struct{ Offered, Forwarded, Selected string }
type fullGatewayOfferKey struct{}

// The native Home accepts WSS only. This short-lived HTTPS proxy terminates a
// synthetic certificate, rewrites the public Host expected by either gateway,
// and tunnels the upgraded socket to its isolated HTTP listener.
func fullGatewayTLSProxy(t *testing.T, address string, ca *x509.Certificate, caKey *ecdsa.PrivateKey, stripOffer bool) (string, func() []fullGatewayNegotiation) {
	t.Helper()
	leafKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	leaf := &x509.Certificate{SerialNumber: big.NewInt(2), Subject: pkix.Name{CommonName: "HMux synthetic loopback"}, NotBefore: time.Now().Add(-time.Hour), NotAfter: ca.NotAfter, DNSNames: []string{"localhost"}, IPAddresses: []net.IP{net.ParseIP("127.0.0.1")}, KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	leafDER, err := x509.CreateCertificate(rand.Reader, leaf, ca, &leafKey.PublicKey, caKey)
	if err != nil {
		t.Fatal(err)
	}
	privateDER, err := x509.MarshalECPrivateKey(leafKey)
	if err != nil {
		t.Fatal(err)
	}
	serverCert, err := tls.X509KeyPair(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: leafDER}), pem.EncodeToMemory(&pem.Block{Type: "EC PRIVATE KEY", Bytes: privateDER}))
	if err != nil {
		t.Fatal(err)
	}
	target, err := url.Parse("http://" + address)
	if err != nil {
		t.Fatal(err)
	}
	proxy := httputil.NewSingleHostReverseProxy(target)
	proxy.Transport = &http.Transport{DialContext: func(ctx context.Context, network, destination string) (net.Conn, error) {
		if destination != address {
			return nil, errors.New("test rejects non-loopback proxy destination")
		}
		return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, address)
	}}
	baseDirector := proxy.Director
	var mu sync.Mutex
	var negotiations []fullGatewayNegotiation
	proxy.Director = func(r *http.Request) {
		baseDirector(r)
		r.Host = "hmux.example"
		if r.URL.Path != "/connect" {
			return
		}
		entry := fullGatewayNegotiation{Offered: r.Header.Get("Sec-WebSocket-Protocol")}
		if stripOffer {
			r.Header.Del("Sec-WebSocket-Protocol")
		}
		entry.Forwarded = r.Header.Get("Sec-WebSocket-Protocol")
		*r = *r.WithContext(context.WithValue(r.Context(), fullGatewayOfferKey{}, entry))
	}
	proxy.ModifyResponse = func(r *http.Response) error {
		if r.Request.URL.Path == "/connect" && r.StatusCode == http.StatusSwitchingProtocols {
			if entry, ok := r.Request.Context().Value(fullGatewayOfferKey{}).(fullGatewayNegotiation); ok {
				entry.Selected = r.Header.Get("Sec-WebSocket-Protocol")
				mu.Lock()
				negotiations = append(negotiations, entry)
				mu.Unlock()
			}
		}
		return nil
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	server := &http.Server{Handler: proxy, ReadHeaderTimeout: 5 * time.Second, IdleTimeout: 30 * time.Second, MaxHeaderBytes: 8192, TLSConfig: &tls.Config{Certificates: []tls.Certificate{serverCert}, MinVersion: tls.VersionTLS12}}
	done := make(chan error, 1)
	go func() { done <- server.Serve(tls.NewListener(listener, server.TLSConfig)) }()
	t.Cleanup(func() {
		_ = server.Close()
		select {
		case err := <-done:
			if !errors.Is(err, http.ErrServerClosed) {
				t.Errorf("synthetic TLS proxy: %v", err)
			}
		case <-time.After(5 * time.Second):
			t.Error("synthetic TLS proxy did not stop")
		}
	})
	return "wss://" + listener.Addr().String() + "/connect", func() []fullGatewayNegotiation {
		mu.Lock()
		defer mu.Unlock()
		return append([]fullGatewayNegotiation(nil), negotiations...)
	}
}

func fullGatewayTransport(address string) *http.Transport {
	return &http.Transport{DialContext: func(ctx context.Context, network, target string) (net.Conn, error) {
		host, _, err := net.SplitHostPort(address)
		if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() || target != "hmux.example:80" {
			return nil, errors.New("test rejects non-loopback/external destination")
		}
		return (&net.Dialer{}).DialContext(ctx, network, address)
	}}
}

func TestRustFullGatewayGoHome(t *testing.T) {
	address := os.Getenv("HMUX_GO_E2E_ADDRESS")
	if address == "" {
		t.Skip("subprocess only")
	}
	root := os.Getenv("HMUX_GO_E2E_ROOT")
	if !filepath.IsAbs(root) || !strings.HasPrefix(filepath.Base(root), "hmux-e2e-go-home-") {
		t.Fatal("isolated root required")
	}
	endpoint := "ws://hmux.example/connect"
	http.DefaultTransport = fullGatewayTransport(address)
	if wss := os.Getenv("HMUX_GO_E2E_WSS"); wss != "" {
		// Go's macOS native verifier does not load SSL_CERT_FILE. Keep the
		// production connector workers and TLS verification, injecting the
		// synthetic trust pool only into this owned test subprocess.
		parsed, err := url.Parse(wss)
		if err != nil || parsed.Scheme != "wss" || parsed.Path != "/connect" || parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" || parsed.Hostname() != "127.0.0.1" || parsed.Port() == "" {
			t.Fatal("invalid synthetic WSS endpoint")
		}
		pemBytes, err := os.ReadFile(filepath.Join(root, "root.pem"))
		if err != nil {
			t.Fatal(err)
		}
		roots := x509.NewCertPool()
		if !roots.AppendCertsFromPEM(pemBytes) {
			t.Fatal("invalid synthetic CA")
		}
		transport := &http.Transport{
			TLSClientConfig: &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12},
			DialContext: func(ctx context.Context, network, target string) (net.Conn, error) {
				if target != parsed.Host {
					return nil, errors.New("test rejects external destination")
				}
				return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, network, target)
			},
		}
		defer transport.CloseIdleConnections()
		http.DefaultTransport = transport
		endpoint = wss
	}
	// All non-loopback attempts are forbidden, even if a collector adds an endpoint.
	cfg := config.HomeConfig{SchemaVersion: 1, Role: "home", InventoryPath: filepath.Join(root, "inventory.toml"), StateDir: filepath.Join(root, "state")}
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGTERM)
	defer cancel()
	err := connectOnce(ctx, endpoint, fullGatewayToken, cfg)
	if ctx.Err() == nil {
		t.Fatalf("actual synthetic Home unexpectedly stopped: %v; cause: %v", err, errors.Unwrap(err))
	}
}

func TestRustFullGatewayTool(t *testing.T) {
	tool := os.Getenv("HMUX_GO_E2E_TOOL")
	if tool == "" {
		t.Skip("synthetic process tool only")
	}
	if os.Getenv("GOGC") != "" || os.Getenv("GOMEMLIMIT") != "" {
		// Native Go tuning must not also tune the synthetic echo/tool workload.
		os.Exit(91)
	}
	root := os.Getenv("HMUX_GO_E2E_ROOT")
	if !filepath.IsAbs(root) || !strings.HasPrefix(filepath.Base(root), "hmux-e2e-go-home-") {
		os.Exit(80)
	}
	if tool == "ps" || tool == "lsof" {
		os.Exit(0)
	}
	var args []string
	for i, value := range os.Args {
		if value == "--" {
			args = os.Args[i+1:]
			break
		}
	}
	if tool != "tmux" || len(args) == 0 {
		os.Exit(81)
	}
	fullGatewayRecordCommand(root, args)
	field := func(flag string) string {
		for i, a := range args {
			if a == flag && i+1 < len(args) {
				return args[i+1]
			}
		}
		return ""
	}
	sep := "|:hmux-sep-v1:|"
	switch args[0] {
	case "list-sessions":
		fmt.Println(strings.Join([]string{"$7", "hmux-e2e-synthetic", "42", "42", "0", "1", "", "0"}, sep))
	case "list-windows":
		fmt.Println(strings.Join([]string{"$7", "shell", "1", filepath.Join(root, "home"), "sh", "80", "24", "12345"}, sep))
	case "list-panes":
		fmt.Println(strings.Join([]string{"$7", "@1", "0", "shell", "b25d,80x24,0,0,1", "1", "%1", "0", "1", filepath.Join(root, "home"), "12345"}, "|:hmux-recovery-v1:|"))
	case "display-message":
		if field("-t") != "$7" {
			os.Exit(83)
		}
		fmt.Println("42")
	case "new-session":
		name := field("-s")
		if !strings.HasPrefix(name, "hmux-app-view-") || field("-t") != "$7" {
			os.Exit(84)
		}
		owner := strings.TrimPrefix(field("-e"), "HMUX_VIEW_OWNER=")
		if owner != "" && (len(owner) != 24 || !strings.HasSuffix(name, "-"+owner)) {
			os.Exit(84)
		}
		if os.WriteFile(filepath.Join(root, "view-owners", name), []byte(owner), 0600) != nil {
			os.Exit(85)
		}
		if os.WriteFile(filepath.Join(root, "views", name), []byte("owned"), 0600) != nil {
			os.Exit(85)
		}
	case "set-hook":
		name := field("-t")
		owner, err := os.ReadFile(filepath.Join(root, "view-owners", name))
		condition := "#{&&:#{==:#{@hmux_app_view},1},#{==:#{session_attached},0}}"
		if len(owner) > 0 {
			condition = "#{&&:" + fullGatewayOwnedCondition(name, string(owner)) + ",#{==:#{session_attached},0}}"
		}
		want := "if-shell -F -t " + name + " '" + condition + "' 'kill-session -t " + name + "'"
		if err != nil || !strings.HasPrefix(name, "hmux-app-view-") || len(args) != 5 || args[3] != "client-detached" || args[4] != want {
			os.Exit(86)
		}
		if os.WriteFile(filepath.Join(root, "view-hooks", name), []byte("verified"), 0600) != nil {
			os.Exit(86)
		}
	case "if-shell":
		name := field("-t")
		owner, err := os.ReadFile(filepath.Join(root, "view-owners", name))
		if err != nil || len(owner) != 24 || !strings.HasPrefix(name, "hmux-app-view-") || len(args) != 6 || args[1] != "-F" || args[4] != fullGatewayOwnedCondition(name, string(owner)) || args[5] != "kill-session -t "+name {
			os.Exit(87)
		}
		_ = os.Remove(filepath.Join(root, "views", name))
	case "kill-session":
		name := field("-t")
		if !strings.HasPrefix(name, "hmux-app-view-") {
			os.Exit(87)
		}
		_ = os.Remove(filepath.Join(root, "views", name))
	case "attach-session":
		if !strings.HasPrefix(field("-t"), "hmux-app-view-") {
			os.Exit(88)
		}
		if os.WriteFile(filepath.Join(root, "views", field("-t")), []byte(strconv.Itoa(os.Getpid())), 0600) != nil {
			os.Exit(88)
		}
		fmt.Println("HMUX-READY")
		scanner := bufio.NewScanner(os.Stdin)
		for scanner.Scan() {
			switch scanner.Text() {
			case "size":
				size, err := unix.IoctlGetWinsize(0, unix.TIOCGWINSZ)
				if err != nil {
					os.Exit(89)
				}
				fmt.Printf("SIZE=%dx%d\n", size.Col, size.Row)
			case "burst":
				_, _ = io.WriteString(os.Stdout, strings.Repeat("x", 768<<10))
				fmt.Println("BURST-END")
			default:
				fmt.Println("INPUT=" + scanner.Text())
			}
		}
	default:
		os.Exit(90)
	}
	os.Exit(0)
}

func fullGatewayOwnedCondition(name, owner string) string {
	return "#{&&:#{==:#{session_name}," + name + "},#{==:#{HMUX_VIEW_OWNER}," + owner + "}}"
}

// Pause ordinary native trust loading with a temporary FIFO. Opening its writer
// proves startup is underway; SIGTERM must be handled before the PEM is released.
func assertRustGatewayStartupSignal(t *testing.T, arguments, env []string, root string) {
	t.Helper()
	fifo := filepath.Join(root, "startup-ca.fifo")
	if err := unix.Mkfifo(fifo, 0600); err != nil {
		t.Fatal(err)
	}
	child := exec.Command(arguments[0], arguments[1:]...)
	for _, entry := range env {
		if !strings.HasPrefix(entry, "SSL_CERT_FILE=") {
			child.Env = append(child.Env, entry)
		}
	}
	child.Env = append(child.Env, "SSL_CERT_FILE="+fifo)
	var logs bytes.Buffer
	child.Stderr = &logs
	if err := child.Start(); err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- child.Wait() }()
	stopped := false
	defer func() {
		if !stopped {
			_ = child.Process.Kill()
			<-done
		}
	}()
	var writer *os.File
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		file, err := os.OpenFile(fifo, os.O_WRONLY|syscall.O_NONBLOCK, 0)
		if err == nil {
			writer = file
			break
		}
		if !errors.Is(err, syscall.ENXIO) {
			t.Fatal(err)
		}
		time.Sleep(10 * time.Millisecond)
	}
	if writer == nil {
		t.Fatal("startup did not enter trust loader")
	}
	defer writer.Close()
	if err := child.Process.Signal(syscall.SIGTERM); err != nil {
		t.Fatal(err)
	}
	pemBytes, err := os.ReadFile(filepath.Join(root, "root.pem"))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := writer.Write(pemBytes); err != nil {
		t.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		stopped = true
		if err != nil {
			t.Fatalf("startup SIGTERM failed: %v: %s", err, logs.String())
		}
		if strings.Contains(logs.String(), "listener ready") {
			t.Fatal("terminated startup began serving")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("startup SIGTERM did not drain initialized owners")
	}
}

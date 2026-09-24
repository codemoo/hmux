// Synthetic-only benchmark driver. The measured process is a separate gateway;
// no local tmux, provider, personal configuration or public endpoint is touched.
package main

import (
	"context"
	"crypto/hmac"
	"crypto/sha1"
	"crypto/sha256"
	"encoding/base32"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/webgateway"
	"github.com/coder/websocket"
)

const host = "hmux.example"
const origin = "https://" + host

type sample struct {
	ElapsedMS int64  `json:"elapsed_ms"`
	RSS       *int64 `json:"rss_bytes"`
	PSS       *int64 `json:"pss_bytes"`
	Threads   *int64 `json:"threads"`
	FDs       *int64 `json:"fd_count"`
}
type report struct {
	Schema         int      `json:"schema_version"`
	Status         string   `json:"status"`
	Scenario       string   `json:"scenario"`
	BinaryHash     string   `json:"binary_sha256"`
	Runtime        string   `json:"driver_toolchain"`
	Gateway        string   `json:"gateway_implementation"`
	HomeProtocol   string   `json:"home_protocol"`
	OS             string   `json:"os"`
	Architecture   string   `json:"architecture"`
	CatalogEntries int      `json:"catalog_entries"`
	Views          int      `json:"active_views"`
	WarmupMS       int64    `json:"warmup_ms"`
	DurationMS     int64    `json:"duration_ms"`
	ReadinessMS    int64    `json:"readiness_ms"`
	GoGC           string   `json:"gogc"`
	GoMemoryLimit  string   `json:"gomemlimit"`
	Boundary       string   `json:"process_boundary"`
	Limitations    []string `json:"limitations"`
	Samples        []sample `json:"samples"`
}
type transport struct{ base http.RoundTripper }

func (t transport) RoundTrip(r *http.Request) (*http.Response, error) {
	c := r.Clone(r.Context())
	c.Host = host
	return t.base.RoundTrip(c)
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "benchmark failed:", err)
		os.Exit(1)
	}
}
func run() error {
	binaryPath := flag.String("binary", "", "explicit gateway candidate path")
	gateway := flag.String("gateway", "go", "gateway implementation: go, rust (native serve), or rust-candidate (test entrypoint)")
	output := flag.String("output", "", "new result JSON path (required)")
	entries := flag.Int("catalog", 100, "synthetic catalog entries; 0 selects S00")
	views := flag.Int("views", 0, "quiet synthetic views, 0..8")
	duration := flag.Duration("duration", 10*time.Second, "sample interval duration")
	warmup := flag.Duration("warmup", time.Second, "warmup after readiness")
	gc := flag.String("gogc", "", "Go candidate only: explicit GOGC override")
	memory := flag.String("gomemlimit", "", "Go candidate only: explicit GOMEMLIMIT override")
	flag.Parse()
	if (*gateway != "go" && *gateway != "rust" && *gateway != "rust-candidate") || (*gateway != "go" && (*gc != "" || *memory != "")) {
		return errors.New("invalid gateway implementation or Go-only tuning options")
	}
	if flag.NArg() != 0 || *binaryPath == "" || *output == "" || *entries < 0 || *entries > 1000 || *views < 0 || *views > 8 || *views > *entries || *duration < time.Second || *duration > 24*time.Hour || *warmup < 0 || *warmup > time.Hour {
		return errors.New("invalid benchmark arguments")
	}
	executable, err := filepath.Abs(*binaryPath)
	if err != nil {
		return err
	}
	binaryData, err := os.ReadFile(executable)
	if err != nil {
		return err
	}
	digest := sha256.Sum256(binaryData)
	binaryData = nil
	// Reserve a fresh output before starting any child; never overwrite evidence.
	resultFile, err := os.OpenFile(*output, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return err
	}
	defer resultFile.Close()
	result := report{Schema: 1, Status: "failed", BinaryHash: hex.EncodeToString(digest[:]), Runtime: runtime.Version(), OS: runtime.GOOS, Architecture: runtime.GOARCH, CatalogEntries: *entries, Views: *views, WarmupMS: warmup.Milliseconds(), DurationMS: duration.Milliseconds(), GoGC: *gc, GoMemoryLimit: *memory, Boundary: "gateway child PID only; excludes driver, Nginx, browser, Home, tmux and providers", Limitations: []string{"loopback HTTP/WS with production Host/Origin/auth gates; excludes TLS/proxy cost", "synthetic Home and quiet views, not physical browser rendering", "sampled RSS/PSS peaks can miss short bursts; no allocation or cgroup profile", "macOS PSS/thread/FD metrics unavailable in this driver; null is not zero", "CPU/latency/soak workloads are not implemented by this driver"}}
	result.Scenario = "S01"
	result.Gateway = *gateway
	result.HomeProtocol = "json-v1"
	result.Limitations = append(result.Limitations, "Rust candidate eagerly loads native TLS trust; Go initializes outbound trust on demand", "quiet gateway measurements do not imply whole-product performance parity")
	if *entries == 0 {
		result.Scenario = "S00"
	}
	if *views > 0 {
		result.Scenario = "S02"
	}
	defer func() { _ = json.NewEncoder(resultFile).Encode(result) }()
	root, err := os.MkdirTemp("", "hmux-bench-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(root)
	root, err = filepath.EvalSymlinks(root)
	if err != nil {
		return err
	}
	assets := filepath.Join(root, "assets")
	if err = os.Mkdir(assets, 0700); err != nil {
		return err
	}
	if err = os.WriteFile(filepath.Join(assets, "index.html"), []byte("<!doctype html><title>Synthetic HMux benchmark</title>"), 0600); err != nil {
		return err
	}
	credentials, err := webgateway.NewCredentials("synthetic-benchmark", "synthetic-password-123")
	if err != nil {
		return err
	}
	credentialPath := filepath.Join(root, "credentials.json")
	if err = webgateway.WriteCredentials(credentialPath, credentials); err != nil {
		return err
	}
	token := webgateway.RandomToken()
	tokenPath := filepath.Join(root, "connector.token")
	if err = os.WriteFile(tokenPath, []byte(token+"\n"), 0600); err != nil {
		return err
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	address := listener.Addr().String()
	_ = listener.Close()
	role := "serve"
	if *gateway == "rust-candidate" {
		role = "--experimental-gateway"
	}
	child := exec.Command(executable, role, "--credentials", credentialPath, "--token-file", tokenPath, "--origin", origin, "--listen", address, "--assets", assets)
	child.Stdout = io.Discard
	debugPath := filepath.Join(root, "child-stderr.log")
	debugFile, err := os.OpenFile(debugPath, os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer debugFile.Close()
	child.Stderr = debugFile
	// No inherited credentials, proxy/TLS overrides or provider paths. Both roles
	// receive identical synthetic Home and native OS trust defaults.
	child.Env = []string{"HOME=" + root, "PATH=/usr/bin:/bin", "LANG=C", "TZ=UTC"}
	if *gc != "" {
		child.Env = append(child.Env, "GOGC="+*gc)
	}
	if *memory != "" {
		child.Env = append(child.Env, "GOMEMLIMIT="+*memory)
	}
	start := time.Now()
	if err = child.Start(); err != nil {
		return err
	}
	exited := make(chan error, 1)
	go func() { exited <- child.Wait() }()
	defer func() {
		_ = child.Process.Signal(syscall.SIGTERM)
		select {
		case <-exited:
		case <-time.After(6 * time.Second):
			_ = child.Process.Kill()
			<-exited
		}
	}()
	ctx, cancel := context.WithTimeout(context.Background(), *warmup+*duration+30*time.Second)
	defer cancel()
	base := http.DefaultTransport.(*http.Transport).Clone()
	base.Proxy = nil
	defer base.CloseIdleConnections()
	client := &http.Client{Transport: transport{base}, Timeout: 5 * time.Second}
	url := "http://" + address
	for {
		req, _ := http.NewRequestWithContext(ctx, "GET", url+"/", nil)
		response, e := client.Do(req)
		if e == nil {
			_, _ = io.Copy(io.Discard, response.Body)
			_ = response.Body.Close()
			if response.StatusCode == 200 {
				break
			}
		}
		if time.Since(start) > 10*time.Second {
			debug, _ := os.Open(debugPath)
			if debug != nil {
				defer debug.Close()
				raw, _ := io.ReadAll(io.LimitReader(debug, 4096))
				return fmt.Errorf("gateway did not become ready: %s", raw)
			}
			return errors.New("gateway did not become ready")
		}
		time.Sleep(20 * time.Millisecond)
	}
	result.ReadinessMS = time.Since(start).Milliseconds()
	var peers []*websocket.Conn
	defer func() {
		for _, c := range peers {
			_ = c.CloseNow()
		}
	}()
	if *entries > 0 {
		home, _, err := websocket.Dial(ctx, "ws://"+address+"/connect", &websocket.DialOptions{HTTPClient: client, HTTPHeader: http.Header{"Authorization": {"Bearer " + token}}})
		if err != nil {
			return err
		}
		peers = append(peers, home)
		home.SetReadLimit(4 << 20)
		go fakeHome(ctx, home, *entries)
	}
	loginBody, _ := json.Marshal(map[string]string{"username": credentials.Username, "password": "synthetic-password-123", "code": totp(credentials.TOTPSecret, time.Now())})
	request, _ := http.NewRequestWithContext(ctx, "POST", url+"/api/login", strings.NewReader(string(loginBody)))
	request.Header.Set("Origin", origin)
	request.Header.Set("Content-Type", "application/json")
	response, err := client.Do(request)
	if err != nil {
		return err
	}
	_ = response.Body.Close()
	if response.StatusCode != 200 || len(response.Cookies()) == 0 {
		return errors.New("synthetic login failed")
	}
	cookie := response.Cookies()[0]
	stateDeadline := time.Now().Add(5 * time.Second)
	for {
		if err = checkState(ctx, client, url, cookie, *entries > 0); err == nil {
			break
		}
		if time.Now().After(stateDeadline) {
			return err
		}
		time.Sleep(10 * time.Millisecond)
	}
	for i := 0; i < *views; i++ {
		c, _, err := websocket.Dial(ctx, "ws://"+address+"/api/terminal", &websocket.DialOptions{HTTPClient: client, HTTPHeader: http.Header{"Origin": {origin}, "Cookie": {cookie.String()}}})
		if err != nil {
			return err
		}
		peers = append(peers, c)
		raw, _ := json.Marshal(webgateway.Message{Type: "open", Session: model.SessionIdentity{ID: fmt.Sprintf("$%d", i+1), CreatedAt: 42}, Cols: 80, Rows: 24})
		if err = c.Write(ctx, websocket.MessageText, raw); err != nil {
			return err
		}
		_, raw, err = c.Read(ctx)
		if err != nil {
			return err
		}
		var ready struct {
			Type string `json:"type"`
		}
		if json.Unmarshal(raw, &ready) != nil || ready.Type != "ready" {
			return errors.New("terminal not ready")
		}
		go func() {
			for {
				if _, _, err := c.Read(ctx); err != nil {
					return
				}
			}
		}()
	}
	if err = checkState(ctx, client, url, cookie, *entries > 0); err != nil {
		return err
	}
	time.Sleep(*warmup)
	sampled := time.Now()
	for time.Since(sampled) < *duration {
		s, err := measure(child.Process.Pid)
		if err != nil {
			return err
		}
		s.ElapsedMS = time.Since(sampled).Milliseconds()
		result.Samples = append(result.Samples, s)
		time.Sleep(250 * time.Millisecond)
	}
	if err = checkState(ctx, client, url, cookie, *entries > 0); err != nil {
		return err
	}
	result.Status = "ok"
	return nil
}
func checkState(ctx context.Context, client *http.Client, url string, cookie *http.Cookie, online bool) error {
	req, _ := http.NewRequestWithContext(ctx, "GET", url+"/api/state", nil)
	req.AddCookie(cookie)
	response, err := client.Do(req)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	var state struct {
		Online bool `json:"online"`
	}
	if response.StatusCode != 200 || json.NewDecoder(io.LimitReader(response.Body, 4<<20)).Decode(&state) != nil || state.Online != online {
		return errors.New("gateway readiness/state mismatch")
	}
	return nil
}
func fakeHome(ctx context.Context, c *websocket.Conn, entries int) {
	send := func(value webgateway.Message) error {
		raw, e := json.Marshal(value)
		if e != nil {
			return e
		}
		return c.Write(ctx, websocket.MessageText, raw)
	}
	_ = send(webgateway.Message{Type: "hello", Capabilities: []string{"terminal-output-flow-v1"}})
	catalog := model.Catalog{ProtocolVersion: 1, GeneratedAt: time.Now().UTC(), Sessions: []model.Session{}}
	for i := 0; i < entries; i++ {
		catalog.Sessions = append(catalog.Sessions, model.Session{ID: fmt.Sprintf("$%d", i+1), CreatedAt: 42, Name: fmt.Sprintf("synthetic-%d", i+1), WindowCount: 1, WindowNames: []string{"shell"}, CurrentCommand: "sh", CurrentPath: "/synthetic/workspace"})
	}
	raw, _ := json.Marshal(catalog)
	if send(webgateway.Message{Type: "catalog", Payload: raw}) != nil {
		return
	}
	go func() {
		tick := time.NewTicker(5 * time.Second)
		defer tick.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-tick.C:
				if send(webgateway.Message{Type: "catalog", Payload: raw}) != nil {
					return
				}
			}
		}
	}()
	for {
		_, raw, err := c.Read(ctx)
		if err != nil {
			return
		}
		var m webgateway.Message
		if json.Unmarshal(raw, &m) != nil {
			return
		}
		if m.Type == "open" || m.Type == "request" {
			if send(webgateway.Message{Type: "response", ID: m.ID, Payload: json.RawMessage(`{"ok":true}`)}) != nil {
				return
			}
		}
	}
}
func totp(secret string, now time.Time) string {
	key, _ := base32.StdEncoding.WithPadding(base32.NoPadding).DecodeString(secret)
	var b [8]byte
	binary.BigEndian.PutUint64(b[:], uint64(now.Unix()/30))
	mac := hmac.New(sha1.New, key)
	_, _ = mac.Write(b[:])
	sum := mac.Sum(nil)
	offset := sum[len(sum)-1] & 15
	return fmt.Sprintf("%06d", (binary.BigEndian.Uint32(sum[offset:offset+4])&0x7fffffff)%1000000)
}
func measure(pid int) (sample, error) {
	var s sample
	if runtime.GOOS == "linux" {
		data, err := os.ReadFile(fmt.Sprintf("/proc/%d/smaps_rollup", pid))
		if err != nil {
			return s, err
		}
		for _, line := range strings.Split(string(data), "\n") {
			fields := strings.Fields(line)
			if len(fields) < 2 {
				continue
			}
			n, err := strconv.ParseInt(fields[1], 10, 64)
			if err != nil {
				continue
			}
			n *= 1024
			switch fields[0] {
			case "Rss:":
				s.RSS = &n
			case "Pss:":
				s.PSS = &n
			}
		}
		if tasks, e := os.ReadDir(fmt.Sprintf("/proc/%d/task", pid)); e == nil {
			n := int64(len(tasks))
			s.Threads = &n
		}
		if fds, e := os.ReadDir(fmt.Sprintf("/proc/%d/fd", pid)); e == nil {
			n := int64(len(fds))
			s.FDs = &n
		}
	} else if runtime.GOOS == "darwin" {
		out, err := exec.Command("/bin/ps", "-o", "rss=", "-p", strconv.Itoa(pid)).Output()
		if err != nil {
			return s, err
		}
		n, err := strconv.ParseInt(strings.TrimSpace(string(out)), 10, 64)
		if err != nil {
			return s, err
		}
		n *= 1024
		s.RSS = &n
	} else {
		return s, errors.New("unsupported measurement OS")
	}
	if s.RSS == nil {
		return s, errors.New("RSS unavailable")
	}
	return s, nil
}

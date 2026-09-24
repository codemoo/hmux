package webgateway

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/coder/websocket"
)

// The full gateway fixture owns the private HOME, native child processes and
// disposable PTYs. These paths never reference installed provider storage.
type fullGatewayActivityFiles struct {
	claudeRoot, codexRoot string
	claude, codex         []string
}

const (
	activityFilesPerProvider = 512
	activityLinesPerFile     = 32
	activityBurstFiles       = 8
	activityBurstLines       = 64
)

func fullGatewayActivityLine(provider string, index, tokens int, stamp string) []byte {
	if provider == "claude" {
		return []byte(fmt.Sprintf(`{"type":"assistant","timestamp":%q,"sessionId":"synthetic-%d","message":{"model":"synthetic","usage":{"input_tokens":%d,"output_tokens":0}}}`+"\n", stamp, index, tokens))
	}
	return []byte(fmt.Sprintf(`{"type":"event_msg","timestamp":%q,"payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":%d,"cached_input_tokens":0,"output_tokens":0}}}}`+"\n", stamp, tokens))
}

func fullGatewayActivityWrite(t *testing.T, path string, line []byte, count int, flag int) {
	t.Helper()
	file, err := os.OpenFile(path, flag, 0600)
	if err != nil {
		t.Fatal(err)
	}
	data := bytes.Repeat(line, count)
	if n, err := file.Write(data); err != nil || n != len(data) {
		_ = file.Close()
		t.Fatalf("activity fixture write: %d/%d bytes: %v", n, len(data), err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
}

func fullGatewayActivitySeed(t *testing.T, home string) fullGatewayActivityFiles {
	t.Helper()
	files := fullGatewayActivityFiles{
		claudeRoot: filepath.Join(home, ".claude", "projects"),
		codexRoot:  filepath.Join(home, ".codex", "sessions"),
		claude:     make([]string, 0, activityFilesPerProvider),
		codex:      make([]string, 0, activityFilesPerProvider),
	}
	stamp := time.Now().UTC().Format(time.RFC3339)
	for _, provider := range []struct {
		name  string
		root  string
		paths *[]string
	}{{"claude", files.claudeRoot, &files.claude}, {"codex", files.codexRoot, &files.codex}} {
		for index := 0; index < activityFilesPerProvider; index++ {
			folder := filepath.Join(provider.root, fmt.Sprintf("project-%04d", index/16))
			if err := os.MkdirAll(folder, 0700); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(folder, fmt.Sprintf("session-%04d.jsonl", index))
			fullGatewayActivityWrite(t, path, fullGatewayActivityLine(provider.name, index, 1, stamp), activityLinesPerFile, os.O_CREATE|os.O_EXCL|os.O_WRONLY)
			*provider.paths = append(*provider.paths, path)
		}
	}
	return files
}

type fullGatewayActivityFixture struct {
	files               fullGatewayActivityFiles
	gatewayPID, homePID int
	open                func() (*websocket.Conn, *int)
	readUntil           func(*websocket.Conn, *int, string) []byte
	input               func(*websocket.Conn, string)
	requestState        func() (int, []byte)
	wait                func(string, func() bool)
	noViews             func() bool
}

func fullGatewayActivity(t *testing.T, f fullGatewayActivityFixture) {
	t.Helper()
	const bootstrap = activityFilesPerProvider * activityLinesPerFile
	stamp := time.Now().UTC().Format(time.RFC3339)
	clockTicks := fullGatewayClockTicks(t)
	view, received := f.open()
	defer view.CloseNow()
	f.readUntil(view, received, "HMUX-READY")
	rtts := make([]float64, 0, 256)
	echoes := 0
	echo := func() {
		t.Helper()
		marker := fmt.Sprintf("activity-%04d", echoes)
		started := time.Now()
		f.input(view, marker+"\n")
		f.readUntil(view, received, "INPUT="+marker)
		if len(rtts) >= 1024 {
			t.Fatal("activity echo sample cap reached")
		}
		rtts = append(rtts, float64(time.Since(started).Nanoseconds())/1e6)
		echoes++
	}
	state := func(expected int) bool {
		t.Helper()
		status, raw := f.requestState()
		var snapshot struct {
			Usage map[string]struct {
				Total    int `json:"today_total_tokens"`
				Sessions int `json:"today_sessions"`
				Status   struct {
					DataSource string `json:"data_source"`
				} `json:"status"`
				Sources map[string]struct {
					Total    int `json:"today_total_tokens"`
					Sessions int `json:"today_sessions"`
				} `json:"sources"`
			} `json:"usage"`
		}
		if status != 200 || json.Unmarshal(raw, &snapshot) != nil {
			t.Fatalf("authenticated activity state: status=%d body=%.256s", status, raw)
		}
		for _, provider := range []string{"claude", "codex"} {
			item, ok := snapshot.Usage[provider]
			if !ok || item.Total != expected || item.Sessions != activityFilesPerProvider ||
				item.Status.DataSource != "api+jsonl" ||
				item.Sources["cli"].Total != expected || item.Sources["cli"].Sessions != activityFilesPerProvider {
				return false
			}
		}
		return true
	}
	phase := func(name string, expected int, quiet time.Duration) {
		t.Helper()
		start := time.Now()
		deadline := start.Add(18 * time.Second)
		for {
			got := state(expected)
			if quiet > 0 && !got {
				t.Fatalf("%s replayed or lost activity; expected %d tokens/provider", name, expected)
			}
			if got && (quiet == 0 || time.Since(start) >= quiet) {
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("%s did not reach %d tokens/provider within 18s", name, expected)
			}
			echo()
			time.Sleep(150 * time.Millisecond)
		}
		fullGatewayPerfResourceSample(t, "activity-"+name, f.gatewayPID, f.homePID, clockTicks)
		t.Logf("native-activity-phase %s", fullGatewayActivityJSON(t, map[string]any{
			"phase": name, "tokens_per_provider": expected,
			"sessions_per_provider": activityFilesPerProvider,
			"echoes":                echoes, "elapsed_seconds": time.Since(start).Seconds(),
		}))
	}
	phase("backfill", bootstrap, 0)
	phase("quiet", bootstrap, 6*time.Second) // Crosses the Home's five-second scan cadence.
	for _, provider := range []struct {
		name  string
		paths []string
	}{{"claude", f.files.claude}, {"codex", f.files.codex}} {
		for index := 0; index < activityBurstFiles; index++ {
			fullGatewayActivityWrite(t, provider.paths[index], fullGatewayActivityLine(provider.name, index, 2, stamp), activityBurstLines, os.O_WRONLY|os.O_APPEND)
		}
	}
	burst := bootstrap + activityBurstFiles*activityBurstLines*2
	phase("append", burst, 0)
	for _, provider := range []struct {
		name  string
		paths []string
	}{{"claude", f.files.claude}, {"codex", f.files.codex}} {
		path := provider.paths[0]
		replacement := path + ".replacement"
		fullGatewayActivityWrite(t, replacement, fullGatewayActivityLine(provider.name, 0, 3, stamp), 1, os.O_CREATE|os.O_EXCL|os.O_WRONLY)
		if err := os.Rename(replacement, path); err != nil {
			t.Fatal(err)
		}
	}
	phase("inode-replacement", burst+3, 0)
	phase("post-replacement-quiet", burst+3, 6*time.Second)
	_ = view.CloseNow()
	f.wait("activity view retained disposable PTY", f.noViews)
	slices.Sort(rtts)
	percentile := func(percent int) float64 {
		if len(rtts) == 0 {
			t.Fatal("activity workload had no socket echoes")
		}
		return rtts[(len(rtts)*percent+99)/100-1]
	}
	t.Logf("native-activity-result %s", fullGatewayActivityJSON(t, map[string]any{
		"files_per_provider": activityFilesPerProvider, "lines_per_file": activityLinesPerFile,
		"burst_files_per_provider": activityBurstFiles, "burst_lines_per_file": activityBurstLines,
		"echoes": echoes, "errors": 0, "clock_ticks_per_second": clockTicks,
		"socket_receipt_rtt_samples_ms": rtts,
		"socket_receipt_rtt_ms":         map[string]float64{"p50": percentile(50), "p95": percentile(95), "p99": percentile(99)},
	}))
}

func fullGatewayActivityJSON(t *testing.T, value any) string {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

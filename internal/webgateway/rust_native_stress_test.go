package webgateway

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
	"testing"
)

func fullGatewayPerfJSON(t *testing.T, value any) string {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

// Linux CPU ticks are converted with the host's actual CLK_TCK. Other systems
// retain a null CPU field rather than pretending that RSS or wall time is CPU.
func fullGatewayClockTicks(t *testing.T) float64 {
	t.Helper()
	if runtime.GOOS != "linux" {
		return 0
	}
	raw, err := exec.Command("getconf", "CLK_TCK").Output()
	if err != nil {
		t.Fatalf("getconf CLK_TCK: %v", err)
	}
	ticks, err := strconv.ParseFloat(strings.TrimSpace(string(raw)), 64)
	if err != nil || ticks <= 0 {
		t.Fatalf("invalid CLK_TCK %q: %v", raw, err)
	}
	return ticks
}

// Each row is one child PID and one bounded checkpoint. Browser, fixture and
// proxy costs are excluded; these samples are not a continuous peak estimate.
func fullGatewayPerfResourceSample(t *testing.T, stage string, gateway, home int, clockTicks float64) {
	t.Helper()
	for _, role := range []struct {
		name string
		pid  int
	}{{"gateway", gateway}, {"home", home}} {
		row := map[string]any{"stage": stage, "role": role.name, "pid": role.pid,
			"rss_bytes": nil, "pss_bytes": nil, "threads": nil, "fds": nil, "cpu_ticks": nil, "cpu_seconds": nil}
		if runtime.GOOS == "linux" {
			raw, err := os.ReadFile(fmt.Sprintf("/proc/%d/smaps_rollup", role.pid))
			if err != nil {
				t.Fatal(err)
			}
			for _, line := range strings.Split(string(raw), "\n") {
				fields := strings.Fields(line)
				if len(fields) < 2 || (fields[0] != "Rss:" && fields[0] != "Pss:") {
					continue
				}
				value, err := strconv.ParseInt(fields[1], 10, 64)
				if err != nil {
					t.Fatal(err)
				}
				row[strings.ToLower(strings.TrimSuffix(fields[0], ":"))+"_bytes"] = value * 1024
			}
			for name, directory := range map[string]string{"threads": "task", "fds": "fd"} {
				entries, err := os.ReadDir(fmt.Sprintf("/proc/%d/%s", role.pid, directory))
				if err != nil {
					t.Fatal(err)
				}
				row[name] = len(entries)
			}
			stat, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", role.pid))
			if err != nil {
				t.Fatal(err)
			}
			end := strings.LastIndexByte(string(stat), ')')
			if end < 0 {
				t.Fatalf("bad proc stat for %s", role.name)
			}
			fields := strings.Fields(string(stat[end+1:]))
			if len(fields) < 13 {
				t.Fatalf("short proc stat for %s", role.name)
			}
			user, userErr := strconv.ParseUint(fields[11], 10, 64)
			system, systemErr := strconv.ParseUint(fields[12], 10, 64)
			if userErr != nil || systemErr != nil {
				t.Fatalf("bad CPU ticks for %s: %v %v", role.name, userErr, systemErr)
			}
			row["cpu_ticks"] = user + system
			row["cpu_seconds"] = float64(user+system) / clockTicks
		} else if runtime.GOOS == "darwin" {
			raw, err := exec.Command("/bin/ps", "-o", "rss=", "-p", strconv.Itoa(role.pid)).Output()
			if err != nil {
				t.Fatal(err)
			}
			value, err := strconv.ParseInt(strings.TrimSpace(string(raw)), 10, 64)
			if err != nil {
				t.Fatal(err)
			}
			row["rss_bytes"] = value * 1024
		}
		if row["rss_bytes"] == nil || (runtime.GOOS == "linux" && row["pss_bytes"] == nil) {
			t.Fatalf("native %s resource unavailable", role.name)
		}
		t.Logf("native-perf-resource %s", fullGatewayPerfJSON(t, row))
	}
}

// Synthetic child processes only. These checkpoints are leak-investigation
// evidence, not a peak-memory, CPU, browser or whole-deployment benchmark.
func fullGatewayResourceSample(t *testing.T, stage string, gateway, home int) {
	t.Helper()
	for _, role := range []struct {
		name string
		pid  int
	}{{"gateway", gateway}, {"home", home}} {
		row := map[string]any{"stage": stage, "role": role.name, "pid": role.pid, "rss_bytes": nil, "pss_bytes": nil, "threads": nil, "fds": nil}
		if runtime.GOOS == "linux" {
			raw, err := os.ReadFile(fmt.Sprintf("/proc/%d/smaps_rollup", role.pid))
			if err != nil {
				t.Fatal(err)
			}
			for _, line := range strings.Split(string(raw), "\n") {
				fields := strings.Fields(line)
				if len(fields) < 2 || (fields[0] != "Rss:" && fields[0] != "Pss:") {
					continue
				}
				value, err := strconv.ParseInt(fields[1], 10, 64)
				if err != nil {
					t.Fatal(err)
				}
				row[strings.ToLower(strings.TrimSuffix(fields[0], ":"))+"_bytes"] = value * 1024
			}
			for name, directory := range map[string]string{"threads": "task", "fds": "fd"} {
				entries, err := os.ReadDir(fmt.Sprintf("/proc/%d/%s", role.pid, directory))
				if err != nil {
					t.Fatal(err)
				}
				row[name] = len(entries)
			}
		} else if runtime.GOOS == "darwin" {
			raw, err := exec.Command("/bin/ps", "-o", "rss=", "-p", strconv.Itoa(role.pid)).Output()
			if err != nil {
				t.Fatal(err)
			}
			value, err := strconv.ParseInt(strings.TrimSpace(string(raw)), 10, 64)
			if err != nil {
				t.Fatal(err)
			}
			row["rss_bytes"] = value * 1024
		}
		if row["rss_bytes"] == nil {
			t.Fatal("native child RSS unavailable")
		}
		raw, err := json.Marshal(row)
		if err != nil {
			t.Fatal(err)
		}
		t.Logf("native-resource %s", raw)
	}
}

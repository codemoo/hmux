package hostmetrics

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// The oracle contains only synthetic command output. The override lets an
// isolated Rust candidate run this test through a Go source overlay.
type metricOracleCase struct {
	Name   string         `json:"name"`
	Kind   string         `json:"kind"`
	First  string         `json:"first,omitempty"`
	Second string         `json:"second,omitempty"`
	Blocks uint64         `json:"blocks,omitempty"`
	Free   uint64         `json:"free,omitempty"`
	Size   uint64         `json:"size,omitempty"`
	Want   *metricOutcome `json:"want"`
}

type metricOutcome struct {
	Percent *float64 `json:"percent,omitempty"`
	Used    *uint64  `json:"used,omitempty"`
	Total   *uint64  `json:"total,omitempty"`
}

const vmBase = "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nAnonymous pages: 100.\nPages wired down: 20.\nPages purgeable: 10.\nPages occupied by compressor: 5.\n"

func metricOracleInputs() []metricOracleCase {
	return []metricOracleCase{
		{Name: "darwin-cpu-final", Kind: "cpu_darwin", First: "CPU usage: 10% user, 10% sys, 80% idle\nCPU usage: 12.5% user, 7.5% sys, 80.0% idle\n"},
		{Name: "darwin-cpu-last-valid", Kind: "cpu_darwin", First: "CPU usage: 20% user, 0% sys, 80% idle\nCPU usage: 30% user, 0% sys, 70% idle\nCPU usage: 0% user, 0% sys, NaN% idle\n"},
		{Name: "darwin-cpu-single", Kind: "cpu_darwin", First: "CPU usage: 1% user, 1% sys, 98% idle\n"},
		{Name: "darwin-cpu-invalid", Kind: "cpu_darwin", First: "CPU usage: 1% user, 1% sys, 98% idle\nCPU usage: 1% user, 1% sys, 101% idle\n"},
		{Name: "darwin-cpu-missing", Kind: "cpu_darwin", First: "Processes: 10 total\n"},

		{Name: "darwin-memory-resident", Kind: "memory_darwin", First: vmBase, Second: "1048576\n"},
		{Name: "darwin-memory-purgeable-clamp", Kind: "memory_darwin", First: "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nAnonymous pages: 100.\nPages wired down: 20.\nPages purgeable: 1000.\nPages occupied by compressor: 5.\n", Second: "1048576"},
		{Name: "darwin-memory-missing", Kind: "memory_darwin", First: "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nAnonymous pages: 100.\nPages wired down: 20.\nPages purgeable: 10.\n", Second: "1048576"},
		{Name: "darwin-memory-too-small-total", Kind: "memory_darwin", First: vmBase, Second: "10"},
		{Name: "darwin-memory-invalid-page", Kind: "memory_darwin", First: "Mach Virtual Memory Statistics: (page size of 0 bytes)\nAnonymous pages: 100.\nPages wired down: 20.\nPages purgeable: 10.\nPages occupied by compressor: 5.\n", Second: "1048576"},

		{Name: "linux-cpu-guest", Kind: "cpu_linux", First: "cpu  100 0 100 700 100 0 0 0 0 0\n", Second: "cpu  150 0 150 780 120 0 0 0 5 0\n"},
		{Name: "linux-cpu-reset", Kind: "cpu_linux", First: "cpu  150 0 150 780 120 0 0 0 5 0\n", Second: "cpu  100 0 100 700 100 0 0 0 0 0\n"},
		{Name: "linux-cpu-unchanged", Kind: "cpu_linux", First: "cpu 1 2 3 4\n", Second: "cpu 1 2 3 4\n"},
		{Name: "linux-cpu-missing", Kind: "cpu_linux", First: "cpu0 1 2 3 4\n", Second: "cpu 1 2 3 4\n"},
		{Name: "linux-cpu-idle-regression", Kind: "cpu_linux", First: "cpu 0 0 0 5\n", Second: "cpu 1 0 0 4\n"},

		{Name: "linux-memory-kibibytes", Kind: "memory_linux", First: "MemTotal: 16000000 kB\nMemFree: 1000 kB\nMemAvailable: 4000000 kB\n"},
		{Name: "linux-memory-raw-bytes", Kind: "memory_linux", First: "MemTotal: 100\nMemAvailable: 25\n"},
		{Name: "linux-memory-missing", Kind: "memory_linux", First: "MemTotal: 10 kB\n"},
		{Name: "linux-memory-overavailable", Kind: "memory_linux", First: "MemTotal: 10 kB\nMemAvailable: 11 kB\n"},
		{Name: "linux-memory-invalid", Kind: "memory_linux", First: "MemTotal: no kB\nMemAvailable: 1 kB\n"},

		{Name: "disk-normal", Kind: "disk_bytes", Blocks: 100, Free: 25, Size: 4096},
		{Name: "disk-empty", Kind: "disk_bytes", Blocks: 0, Free: 0, Size: 4096},
		{Name: "disk-free-exceeds", Kind: "disk_bytes", Blocks: 100, Free: 101, Size: 4096},
		{Name: "disk-zero-size", Kind: "disk_bytes", Blocks: 100, Free: 25, Size: 0},
		{Name: "disk-wire-limit", Kind: "disk_bytes", Blocks: 1 << 53, Free: 0, Size: 1},

		{Name: "gpu-canonical-maximum", Kind: "gpu_darwin", First: `<plist><array><dict><key>GPU Power</key><integer>99</integer><key>GPU Activity(%)</key><integer>88</integer><key>Device Utilization %</key><integer>12</integer><key>Device Utilization %</key><real>34.5</real></dict></array></plist>`},
		{Name: "gpu-activity-fallback", Kind: "gpu_darwin", First: `<plist><dict><key>GPU Activity(%)</key><string>42.25</string></dict></plist>`},
		{Name: "gpu-apple-plist", Kind: "gpu_darwin", First: "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Device Utilization %</key><integer>35</integer></dict></plist>"},
		{Name: "gpu-power-only", Kind: "gpu_darwin", First: `<plist><dict><key>GPU Power</key><integer>75</integer></dict></plist>`},
		{Name: "gpu-nonfinite", Kind: "gpu_darwin", First: `<plist><dict><key>Device Utilization %</key><real>NaN</real></dict></plist>`},
	}
}

func evaluateMetricOracle(c *metricOracleCase) {
	switch c.Kind {
	case "cpu_darwin":
		value, err := parseCPUPercent([]byte(c.First))
		if err == nil && value != nil {
			c.Want = &metricOutcome{Percent: value}
		}
	case "memory_darwin":
		used, total, err := parseMemory([]byte(c.First), []byte(c.Second))
		if err == nil && used != nil && total != nil {
			c.Want = &metricOutcome{Used: used, Total: total}
		}
	case "cpu_linux":
		if value := cpuPercentBetween([]byte(c.First), []byte(c.Second)); value != nil {
			c.Want = &metricOutcome{Percent: value}
		}
	case "memory_linux":
		used, total := parseMeminfo([]byte(c.First))
		if used != nil && total != nil {
			c.Want = &metricOutcome{Used: used, Total: total}
		}
	case "disk_bytes":
		used, total, ok := diskBytes(c.Blocks, c.Free, c.Size)
		if ok {
			c.Want = &metricOutcome{Used: &used, Total: &total}
		}
	case "gpu_darwin":
		value, err := parseGPUPercent([]byte(c.First))
		if err == nil && value != nil {
			c.Want = &metricOutcome{Percent: value}
		}
	default:
		panic("unknown metric oracle kind: " + c.Kind)
	}
}

func TestRustMetricsCompatibilityOracle(t *testing.T) {
	cases := metricOracleInputs()
	for i := range cases {
		evaluateMetricOracle(&cases[i])
	}
	actual, err := json.MarshalIndent(cases, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	actual = append(actual, '\n')
	path := os.Getenv("HMUX_HOSTMETRICS_ORACLE")
	if path == "" {
		path = filepath.Join("..", "..", "tests", "fixtures", "hostmetrics-v1", "go-oracle.json")
	}
	if os.Getenv("HMUX_UPDATE_HOSTMETRICS_ORACLE") == "1" {
		if err := os.WriteFile(path, actual, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(actual, want) {
		t.Fatalf("Go host metrics behavior drifted from %s", path)
	}
}

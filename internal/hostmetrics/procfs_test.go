package hostmetrics

import "testing"

func TestCPUPercentBetweenProcStatSamples(t *testing.T) {
	first := []byte("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3 4\n")
	// +100 busy (user 50, system 50) and +100 idle/iowait over 200 jiffies.
	second := []byte("cpu  150 0 150 780 120 0 0 0 5 0\n")
	got := cpuPercentBetween(first, second)
	if got == nil || *got != 50 {
		t.Fatalf("cpu = %v", got)
	}
	if cpuPercentBetween(second, first) != nil || cpuPercentBetween([]byte("x"), second) != nil {
		t.Fatal("accepted an invalid pair")
	}
}

func TestParseMeminfo(t *testing.T) {
	used, total := parseMeminfo([]byte("MemTotal:       16000000 kB\nMemFree:  1000 kB\nMemAvailable:    4000000 kB\n"))
	if used == nil || total == nil || *total != 16000000*1024 || *used != 12000000*1024 {
		t.Fatalf("used=%v total=%v", used, total)
	}
	if u, tt := parseMeminfo([]byte("MemTotal: 10 kB\n")); u != nil || tt != nil {
		t.Fatal("missing MemAvailable accepted")
	}
}

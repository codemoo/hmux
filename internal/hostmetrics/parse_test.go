package hostmetrics

import (
	"math"
	"strings"
	"testing"
)

func TestParseCPUUsesFinalCompleteSample(t *testing.T) {
	output := []byte("CPU usage: 10% user, 10% sys, 80% idle\nCPU usage: 12.5% user, 7.5% sys, 80.0% idle\n")
	value, err := parseCPUPercent(output)
	if err != nil || value == nil || math.Abs(*value-20) > 1e-9 {
		t.Fatalf("cpu=%v err=%v", value, err)
	}
}

func TestParseCPURejectsMissingAndResetLikeOutOfRangeSamples(t *testing.T) {
	for _, output := range []string{
		"Processes: 10 total\n",
		"CPU usage: 1% user, 1% sys, 98% idle\n",
		"CPU usage: 1% user, 1% sys, 98% idle\nCPU usage: 1% user, 1% sys, 101% idle\n",
		"CPU usage: 1% user, 1% sys, 98% idle\nCPU usage: 1% user, 1% sys, NaN% idle\n",
	} {
		if value, err := parseCPUPercent([]byte(output)); err == nil || value != nil {
			t.Fatalf("invalid CPU sample accepted: value=%v err=%v", value, err)
		}
	}
}

func TestParseMemoryUsesAnonymousWiredAndResidentCompressedPages(t *testing.T) {
	vmStat := []byte(`Mach Virtual Memory Statistics: (page size of 4096 bytes)
Pages free: 10.
Anonymous pages: 100.
Pages wired down: 20.
Pages purgeable: 10.
Pages occupied by compressor: 5.
`)
	used, total, err := parseMemory(vmStat, []byte("1048576\n"))
	if err != nil || used == nil || total == nil || *used != 115*4096 || *total != 1048576 {
		t.Fatalf("used=%v total=%v err=%v", used, total, err)
	}
}

func TestParseMemoryRejectsMissingOverflowAndImpossibleBounds(t *testing.T) {
	valid := `Mach Virtual Memory Statistics: (page size of 4096 bytes)
Anonymous pages: 100.
Pages wired down: 20.
Pages purgeable: 10.
Pages occupied by compressor: 5.
`
	for _, test := range []struct{ vm, total string }{
		{strings.Replace(valid, "Anonymous pages", "Other pages", 1), "1048576"},
		{valid, "10"},
		{strings.Replace(valid, "100.", "18446744073709551615.", 1), "1048576"},
		{valid, "1152921504606846977"},
	} {
		if used, total, err := parseMemory([]byte(test.vm), []byte(test.total)); err == nil || used != nil || total != nil {
			t.Fatalf("invalid memory accepted: used=%v total=%v err=%v", used, total, err)
		}
	}
}

func TestParseGPUUsesCanonicalFieldMaximumAndIgnoresPower(t *testing.T) {
	xml := []byte(`<?xml version="1.0"?><plist><array><dict>
<key>GPU Power</key><integer>99</integer>
<key>GPU Activity(%)</key><integer>88</integer>
<key>Device Utilization %</key><integer>12</integer>
<key>Device Utilization %</key><real>34.5</real>
</dict></array></plist>`)
	value, err := parseGPUPercent(xml)
	if err != nil || value == nil || *value != 34.5 {
		t.Fatalf("gpu=%v err=%v", value, err)
	}
}

func TestParseGPUFallsBackToActivityAndOmitsMissingOrNonfinite(t *testing.T) {
	value, err := parseGPUPercent([]byte(`<plist><dict><key>GPU Activity(%)</key><string>42.25</string></dict></plist>`))
	if err != nil || value == nil || *value != 42.25 {
		t.Fatalf("fallback gpu=%v err=%v", value, err)
	}
	for _, input := range []string{
		`<plist><dict><key>GPU Power</key><integer>75</integer></dict></plist>`,
		`<plist><dict><key>Device Utilization %</key><real>NaN</real></dict></plist>`,
		`<plist><dict><key>Device Utilization %</key><integer>101</integer></dict></plist>`,
	} {
		if value, err := parseGPUPercent([]byte(input)); err == nil || value != nil {
			t.Fatalf("invalid GPU accepted: value=%v err=%v", value, err)
		}
	}
}

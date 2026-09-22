package model

import (
	"encoding/json"
	"math"
	"strings"
	"testing"
	"time"
)

func validInventory() Inventory {
	return Inventory{
		SchemaVersion: 1,
		Revision:      "test",
		Profiles:      []Profile{{ID: "codex", Label: "Codex", DefaultDirectory: "~/work", Command: []string{"codex"}}},
	}
}

func TestInventoryValidate(t *testing.T) {
	if err := validInventory().Validate(); err != nil {
		t.Fatal(err)
	}
	tests := []struct {
		name   string
		mutate func(*Inventory)
	}{
		{"duplicate profile", func(i *Inventory) { i.Profiles = append(i.Profiles, i.Profiles[0]) }},
		{"missing profiles", func(i *Inventory) { i.Profiles = nil }},
		{"invalid profile id", func(i *Inventory) { i.Profiles[0].ID = "bad;id" }},
		{"empty command", func(i *Inventory) { i.Profiles[0].Command = nil }},
		{"control in command", func(i *Inventory) { i.Profiles[0].Command = []string{"sh\n"} }},
		{"control in label", func(i *Inventory) { i.Profiles[0].Label = "bad\n" }},
		{"invalid schema", func(i *Inventory) { i.SchemaVersion = 0 }},
		{"profile directory newline", func(i *Inventory) { i.Profiles[0].DefaultDirectory = "~/work\nHost bad" }},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			value := validInventory()
			tt.mutate(&value)
			if err := value.Validate(); err == nil {
				t.Fatal("expected validation error")
			}
		})
	}
}

func TestValidateSessionID(t *testing.T) {
	for _, value := range []string{"$0", "$123456"} {
		if err := ValidateSessionID(value); err != nil {
			t.Errorf("%q: %v", value, err)
		}
	}
	for _, value := range []string{"name", "$", "$1;touch", "$-1", "$1234567890123"} {
		if err := ValidateSessionID(value); err == nil {
			t.Errorf("%q should fail", value)
		}
	}
}

func TestSafeTextRemovesControls(t *testing.T) {
	if got := SafeText("한글\x1b[31m\tname\n", 100); got != "한글 [31m name" {
		t.Fatalf("got %q", got)
	}
	if got := SafeText("safe\u202eevil", 100); got != "safe evil" {
		t.Fatalf("bidirectional control survived: %q", got)
	}
	if got := SafeText("가나", 4); got != "가" || len(got) > 4 {
		t.Fatalf("byte limit was exceeded: %q (%d bytes)", got, len(got))
	}
}

func TestCatalogOmitsHostMetricsForLegacyPayload(t *testing.T) {
	data, err := json.Marshal(Catalog{ProtocolVersion: ProtocolVersion, Sessions: []Session{}})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(data), "host_metrics") {
		t.Fatalf("legacy catalog contains host_metrics: %s", data)
	}
}

func TestValidateHostMetricsBounds(t *testing.T) {
	percent := 50.0
	used := uint64(4)
	total := uint64(8)
	valid := &HostMetrics{
		ObservedAt: time.Now().UTC(), CPUPercent: &percent,
		MemoryUsedBytes: &used, MemoryTotalBytes: &total,
	}
	if err := ValidateHostMetrics(valid); err != nil {
		t.Fatal(err)
	}
	invalidPercent := math.NaN()
	tooLarge := MaximumHostMemoryBytes + 1
	for name, value := range map[string]*HostMetrics{
		"missing observation time": {CPUPercent: &percent},
		"empty observation":        {ObservedAt: time.Now().UTC()},
		"nonfinite percent":        {ObservedAt: time.Now().UTC(), CPUPercent: &invalidPercent},
		"memory missing total":     {ObservedAt: time.Now().UTC(), MemoryUsedBytes: &used},
		"memory exceeds total":     {ObservedAt: time.Now().UTC(), MemoryUsedBytes: &total, MemoryTotalBytes: &used},
		"memory total too large":   {ObservedAt: time.Now().UTC(), MemoryUsedBytes: &used, MemoryTotalBytes: &tooLarge},
	} {
		t.Run(name, func(t *testing.T) {
			if err := ValidateHostMetrics(value); err == nil {
				t.Fatal("invalid host metrics accepted")
			}
		})
	}
}

func TestCatalogDecodeDropsInvalidOptionalHostMetrics(t *testing.T) {
	for name, metrics := range map[string]string{
		"wrong type":        `"bad"`,
		"invalid timestamp": `{"observed_at":"bad","cpu_percent":20}`,
		"invalid percent":   `{"observed_at":"2026-09-08T12:00:00Z","cpu_percent":100.1}`,
		"unpaired memory":   `{"observed_at":"2026-09-08T12:00:00Z","memory_used_bytes":4}`,
		"unknown field":     `{"observed_at":"2026-09-08T12:00:00Z","cpu_percent":20,"host_id":"private"}`,
	} {
		t.Run(name, func(t *testing.T) {
			data := []byte(`{"protocol_version":1,"generated_at":"2026-09-08T12:00:00Z","sessions":[],"host_metrics":` + metrics + `}`)
			var value Catalog
			decoder := json.NewDecoder(strings.NewReader(string(data)))
			decoder.DisallowUnknownFields()
			if err := decoder.Decode(&value); err != nil {
				t.Fatalf("valid catalog rejected: %v", err)
			}
			if value.HostMetrics == nil || ValidateHostMetrics(value.HostMetrics) == nil {
				t.Fatalf("invalid metrics did not become a fail-open sentinel: %#v", value.HostMetrics)
			}
		})
	}
}

func TestCatalogDecodeKeepsStrictTopLevelSchemaAndValidMetrics(t *testing.T) {
	data := `{"protocol_version":1,"generated_at":"2026-09-08T12:00:00Z","sessions":[],"host_metrics":{"observed_at":"2026-09-08T12:00:00Z","cpu_percent":20}}`
	var value Catalog
	if err := json.Unmarshal([]byte(data), &value); err != nil || value.HostMetrics == nil || value.HostMetrics.CPUPercent == nil {
		t.Fatalf("valid metrics value=%#v err=%v", value, err)
	}
	decoder := json.NewDecoder(strings.NewReader(`{"protocol_version":1,"generated_at":"2026-09-08T12:00:00Z","sessions":[],"unexpected":true}`))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&value); err == nil {
		t.Fatal("unknown top-level catalog field accepted")
	}
}

func TestHostDiskBounds(t *testing.T) {
	used, total := uint64(250), uint64(1000)
	value := &HostMetrics{ObservedAt: time.Now().UTC(), DiskUsedBytes: &used, DiskTotalBytes: &total}
	if err := ValidateHostMetrics(value); err != nil {
		t.Fatal(err)
	}
	value.DiskTotalBytes = nil
	if ValidateHostMetrics(value) == nil {
		t.Fatal("unpaired disk accepted")
	}
	value.DiskTotalBytes = &total
	used = 1001
	if ValidateHostMetrics(value) == nil {
		t.Fatal("used exceeds total")
	}
}

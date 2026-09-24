package webgateway

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"
)

// Keep all inputs synthetic. The state machine gets explicit times; report's
// wall clock is normalized without replacing the actual Go filtering/serializer.
func TestRustDiagnosticsOracle(t *testing.T) {
	base := time.Date(2026, 9, 24, 0, 0, 0, 123456789, time.UTC)
	primary := loginSession{id: "login-primary", username: "primary"}
	other := loginSession{id: "login-other", username: "primary", profile: strings.Repeat("a", 64)}
	fresh := func() *diagnosticStore { return &diagnosticStore{rates: make(map[string]diagnosticRate)} }
	type decodeCase struct {
		JSON  string `json:"json"`
		Valid bool   `json:"valid"`
	}
	var decode []decodeCase
	seed, _ := json.Marshal(diagnosticFixture(base))
	inputs := []string{string(seed), `{}`, `[]`, `null`, string(seed) + ` {}`, strings.Replace(string(seed), `"version":1`, `"version":null`, 1)}
	mutate := func(field string, value any) {
		var body map[string]any
		_ = json.Unmarshal(seed, &body)
		body["events"].([]any)[0].(map[string]any)[field] = value
		raw, _ := json.Marshal(body)
		inputs = append(inputs, string(raw))
	}
	for key, values := range map[string][]any{
		"sequence": {0, -1, 2147483647, 2147483648, nil, "1"}, "at": {base.Add(-diagnosticTTL).UnixMilli(), base.Add(-diagnosticTTL).UnixMilli() - 1, base.Add(5 * time.Minute).UnixMilli(), base.Add(5*time.Minute).UnixMilli() + 1},
		"reason": {nil, "", "SECRET", map[string]any{"network": nil}}, "route": {nil, "other", "SECRET"},
		"code": {-1, 4999, 5000, nil}, "retry_ms": {60000, 60001}, "duration_ms": {86400000, 86400001}, "attempt": {1000000, 1000001}, "line": {10000000, 10000001}, "column": {-1, 0}, "online": {nil, false, "yes"}, "message": {"SECRET"},
	} {
		for _, value := range values {
			mutate(key, value)
		}
	}
	// A deterministic sort makes fixture generation independent of map order.
	sort.Strings(inputs)
	for _, input := range inputs {
		var batch diagnosticBatch
		valid := strictPayload(json.RawMessage(input), &batch) == nil && fresh().append(primary, "Safari on macOS", batch, base) == 202
		decode = append(decode, decodeCase{input, valid})
	}
	// All events in a step differ only by sequence. Keep one template rather
	// than retaining thousands of repeated copies in the checked-in oracle.
	type compactBatch struct {
		Version   int             `json:"version"`
		Client    string          `json:"client"`
		Build     string          `json:"build"`
		Event     diagnosticEvent `json:"event"`
		Sequences []int64         `json:"sequences"`
	}
	type step struct {
		At       time.Time    `json:"at"`
		Login    string       `json:"login"`
		Account  string       `json:"account"`
		Profile  string       `json:"profile"`
		Browser  string       `json:"browser"`
		Batch    compactBatch `json:"batch"`
		Status   int          `json:"status"`
		Count    int          `json:"count"`
		Revision uint64       `json:"revision"`
		Digest   string       `json:"digest"`
	}
	var steps []step
	d := fresh()
	digest := func(rows []diagnosticRecord) string {
		// Canonical key order, preserving integer precision via UseNumber.
		raw, _ := json.Marshal(rows)
		var value any
		decoder := json.NewDecoder(bytes.NewReader(raw))
		decoder.UseNumber()
		_ = decoder.Decode(&value)
		raw, _ = json.Marshal(value)
		sum := sha256.Sum256(raw)
		return hex.EncodeToString(sum[:])
	}
	apply := func(login loginSession, browser string, batch diagnosticBatch, at time.Time) {
		status := d.append(login, browser, batch, at)
		compact := compactBatch{Version: batch.Version, Client: batch.Client, Build: batch.Build, Event: batch.Events[0]}
		for _, event := range batch.Events {
			compact.Sequences = append(compact.Sequences, event.Sequence)
			event.Sequence = compact.Event.Sequence
			if event != compact.Event {
				t.Fatal("cannot compact different event bodies")
			}
		}
		steps = append(steps, step{at, login.id, login.username, login.profile, browser, compact, status, len(d.records), d.revision, digest(d.records)})
	}
	fixture := diagnosticFixture(base)
	apply(primary, "Safari on macOS", fixture, base)
	apply(primary, "Safari on macOS", fixture, base)
	changed := fixture
	changed.Build = "app-new-build.js"
	apply(primary, "Chrome", changed, base)
	apply(other, "Firefox on Linux", fixture, base)
	for i := 0; i < 4; i++ {
		batch := diagnosticFixture(base)
		batch.Client = fmt.Sprintf("12345678-1234-4234-8234-%012d", i)
		apply(primary, "Safari on macOS", batch, base)
	}
	later := base.Add(time.Minute)
	apply(primary, "Safari", diagnosticFixture(later), later)
	for account := 0; account < 10; account++ {
		for group := 0; group < 13; group++ {
			b := diagnosticFixture(later)
			b.Events = nil
			for n := 0; n < 20; n++ {
				e := fixture.Events[0]
				e.At = later.UnixMilli()
				e.Sequence = int64(group*20 + n + 1)
				b.Events = append(b.Events, e)
			}
			apply(loginSession{id: fmt.Sprintf("login-%d-%d", account, group), username: fmt.Sprintf("account-%d", account)}, "Unknown browser", b, later)
		}
		if account == 0 {
			b := diagnosticFixture(later)
			e := b.Events[0]
			e.Sequence = 300
			b.Events = []diagnosticEvent{e, e}
			b.Events[1].Sequence = 5
			apply(loginSession{id: "eviction-replay", username: "account-0"}, "Unknown browser", b, later)
		}
	}
	// Report is collected by the actual owner and its private fields are stripped.
	reportStore := fresh()
	reportStore.records = append([]diagnosticRecord(nil), d.records...)
	reportStore.revision = d.revision
	wall := time.Now().UTC()
	for i := range reportStore.records {
		reportStore.records[i].Received = wall
		reportStore.records[i].At = wall.UnixMilli()
	}
	login := loginSession{username: "account-9"}
	report := reportStore.report(login)
	rows := report["events"].([]diagnosticRecord)
	j := 0
	for _, original := range d.records {
		if original.Account == login.username {
			rows[j].Received = original.Received
			rows[j].At = original.At
			j++
		}
	}
	report["generated_at"] = later
	// The complete record format is represented by a small private snapshot.
	disk, _ := json.Marshal(struct {
		Version int                `json:"version"`
		Records []diagnosticRecord `json:"records"`
	}{1, d.records[len(d.records)-2:]})
	raw, err := json.MarshalIndent(map[string]any{"base": base, "decode": decode, "steps": steps, "report": report, "disk": json.RawMessage(disk)}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "diagnostics-v1", "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_DIAGNOSTICS_FIXTURE") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, raw, 0644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(raw, want) {
		t.Fatal("Go diagnostics oracle changed")
	}
}

// Rust writes the current private history. Go loads, appends, and saves it; a
// subsequent ignored Rust test verifies the latest state instead of an old copy.
func TestRustDiagnosticsHandoff(t *testing.T) {
	root := os.Getenv("HMUX_RUST_DIAGNOSTICS_HANDOFF")
	if root == "" {
		t.Skip("isolated handoff directory supplied by make rust-compat")
	}
	d := newDiagnosticStore(filepath.Join(root, "diagnostics.json"))
	defer d.close()
	login := loginSession{id: "handoff-go", username: "primary"}
	if d.disabled || len(d.records) != 1 || d.records[0].Sequence != 1 {
		t.Fatal("Rust diagnostics not readable by Go")
	}
	batch := diagnosticFixture(time.Now())
	batch.Events[0].Sequence = 2
	if d.append(login, "Firefox", batch, time.Now()) != 202 {
		t.Fatal("Go could not append current diagnostics")
	}
	d.close()
}

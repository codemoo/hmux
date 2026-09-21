package webgateway

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func diagnosticFixture(at time.Time) diagnosticBatch {
	return diagnosticBatch{Version: 1, Client: "12345678-1234-4234-8234-123456789abc", Build: "app-fixture.js", Events: []diagnosticEvent{{Sequence: 1, At: at.UnixMilli(), Kind: "terminal-failed", Reason: "network", Code: 1006, Attempt: 1, RetryMS: 1000, Online: true, Visible: true}}}
}
func testDiagnosticStore(t *testing.T) *diagnosticStore {
	t.Helper()
	d := newDiagnosticStore(filepath.Join(t.TempDir(), "diagnostics.json"))
	t.Cleanup(d.close)
	return d
}
func TestDiagnosticsAccountIsolationDeduplicationAndPrivatePersistence(t *testing.T) {
	d := testDiagnosticStore(t)
	now := time.Now()
	batch := diagnosticFixture(now)
	a := loginSession{id: "a", username: "owner-a", profile: strings.Repeat("1", 64)}
	b := loginSession{id: "b", username: "owner-b", profile: strings.Repeat("2", 64)}
	for _, login := range []loginSession{a, a, b} {
		if got := d.append(login, "Safari on macOS", batch, now); got != 202 {
			t.Fatal(got)
		}
	}
	report := d.report(a)
	rows := report["events"].([]diagnosticRecord)
	if len(rows) != 1 || rows[0].Account != "" || rows[0].Profile != "" {
		t.Fatal("account leak or duplicate", rows)
	}
	if len(d.report(loginSession{username: a.username, profile: "different"})["events"].([]diagnosticRecord)) != 0 {
		t.Fatal("profile isolation failed")
	}
	d.close()
	info, err := os.Stat(d.path)
	if err != nil || info.Mode().Perm() != 0600 {
		t.Fatal("unsafe diagnostics file", err)
	}
	again := newDiagnosticStore(d.path)
	defer again.close()
	if len(again.report(b)["events"].([]diagnosticRecord)) != 1 {
		t.Fatal("lost persisted diagnostics")
	}
	if got := d.append(a, "Safari", batch, now); got != 503 {
		t.Fatal("accepted after close")
	}
}
func TestDiagnosticsStrictHTTPBoundaryAndSecretExclusion(t *testing.T) {
	s := testServer(t)
	token := loginForTest(t, s)
	csrf, _, _ := s.auth.get(token, false)
	batch := diagnosticFixture(time.Now())
	if got := request(s, "POST", "/api/diagnostics", batch, "", "", s.origin).Code; got != 401 {
		t.Fatal(got)
	}
	if got := request(s, "POST", "/api/diagnostics", batch, token, "", s.origin).Code; got != 403 {
		t.Fatal(got)
	}
	if got := request(s, "POST", "/api/diagnostics", batch, token, csrf, "https://other.example").Code; got != 403 {
		t.Fatal(got)
	}
	raw, _ := json.Marshal(batch)
	var extra map[string]any
	_ = json.Unmarshal(raw, &extra)
	extra["message"] = "SECRET terminal contents"
	if got := request(s, "POST", "/api/diagnostics", extra, token, csrf, s.origin).Code; got != 400 {
		t.Fatal("unknown fields accepted", got)
	}
	extra["message"] = strings.Repeat("x", 17000)
	if got := request(s, "POST", "/api/diagnostics", extra, token, csrf, s.origin).Code; got != 400 {
		t.Fatal("oversized body accepted", got)
	}
	if got := request(s, "POST", "/api/diagnostics", batch, token, csrf, s.origin).Code; got != 202 {
		t.Fatal(got)
	}
	got := request(s, "GET", "/api/diagnostics", nil, token, "", "")
	if got.Code != 200 || !strings.Contains(got.Header().Get("Cache-Control"), "no-store") {
		t.Fatal(got.Code)
	}
	for _, secret := range []string{token, csrf, testPassword, "SECRET"} {
		if strings.Contains(got.Body.String(), secret) {
			t.Fatal("secret in diagnostics export")
		}
	}
	s.diagnostics.close()
	stored, err := os.ReadFile(s.diagnostics.path)
	if err != nil {
		t.Fatal(err)
	}
	for _, secret := range []string{token, csrf, testPassword, "SECRET"} {
		if strings.Contains(string(stored), secret) {
			t.Fatal("secret in persisted diagnostics")
		}
	}
	if got := request(s, http.MethodDelete, "/api/diagnostics", nil, token, csrf, s.origin).Code; got != 405 {
		t.Fatal(got)
	}
}
func TestDiagnosticsRejectUnboundedFieldsAndRateLimitUUIDChurn(t *testing.T) {
	d := testDiagnosticStore(t)
	now := time.Now()
	login := loginSession{id: "login", username: "owner"}
	mutations := []func(*diagnosticBatch){
		func(b *diagnosticBatch) { b.Client = "secret" }, func(b *diagnosticBatch) { b.Build = "https://private.example/path" }, func(b *diagnosticBatch) { b.Events[0].Reason = "SECRET" }, func(b *diagnosticBatch) { b.Events[0].Route = "/api/action?token=SECRET" }, func(b *diagnosticBatch) { b.Events[0].Sequence = -1 }, func(b *diagnosticBatch) { b.Events[0].DurationMS = -1 }, func(b *diagnosticBatch) { b.Events[0].RetryMS = 100000 }, func(b *diagnosticBatch) { b.Events[0].At = now.Add(time.Hour).UnixMilli() }, func(b *diagnosticBatch) {
			for len(b.Events) < 21 {
				b.Events = append(b.Events, b.Events[0])
			}
		},
	}
	for _, mutate := range mutations {
		b := diagnosticFixture(now)
		mutate(&b)
		if got := d.append(login, "Unknown browser", b, now); got != 400 {
			t.Fatal(got)
		}
	}
	for i := 0; i < 7; i++ {
		b := diagnosticFixture(now)
		b.Client = fmt.Sprintf("12345678-1234-4234-8234-%012d", i)
		got := d.append(login, "Unknown browser", b, now)
		want := 202
		if i == 6 {
			want = 429
		}
		if got != want {
			t.Fatal(got, want)
		}
	}
	if got := d.append(login, "Unknown browser", diagnosticFixture(now.Add(time.Minute)), now.Add(time.Minute)); got != 202 {
		t.Fatal("rate bucket did not expire", got)
	}
}
func TestDiagnosticsRetentionAndGlobalAccountCaps(t *testing.T) {
	d := testDiagnosticStore(t)
	now := time.Now()
	old := now.Add(-diagnosticTTL - time.Second)
	if d.append(loginSession{id: "old", username: "old"}, "Unknown browser", diagnosticFixture(old), old) != 202 {
		t.Fatal("old fixture rejected")
	}
	if len(d.report(loginSession{username: "old"})["events"].([]diagnosticRecord)) != 0 {
		t.Fatal("expired events exported")
	}
	for account := 0; account < 10; account++ {
		for i := 0; i < 15; i++ {
			b := diagnosticFixture(now)
			b.Events = nil
			for n := 0; n < 20; n++ {
				e := diagnosticFixture(now).Events[0]
				e.Sequence = int64(i*20 + n + 1)
				b.Events = append(b.Events, e)
			}
			login := loginSession{id: fmt.Sprintf("%d-%d", account, i), username: fmt.Sprint(account)}
			if d.append(login, "Unknown browser", b, now) != 202 {
				t.Fatal("fixture rejected")
			}
		}
	}
	if len(d.records) != diagnosticLimit {
		t.Fatal("global cap failed", len(d.records))
	}
	if len(d.report(loginSession{username: "9"})["events"].([]diagnosticRecord)) != diagnosticAccountLimit {
		t.Fatal("account cap failed")
	}
	d.close()
	info, err := os.Stat(d.path)
	if err != nil || info.Size() <= 16<<10 {
		t.Fatal("restart fixture must exceed the wire payload limit", err)
	}
	again := newDiagnosticStore(d.path)
	defer again.close()
	if again.disabled || len(again.records) != diagnosticLimit || len(again.report(loginSession{username: "9"})["events"].([]diagnosticRecord)) != diagnosticAccountLimit {
		t.Fatal("valid full history lost on restart")
	}
}
func TestDiagnosticsConcurrentReplayAndStorageFailure(t *testing.T) {
	d := testDiagnosticStore(t)
	now := time.Now()
	batch := diagnosticFixture(now)
	var wg sync.WaitGroup
	for i := 0; i < 5; i++ {
		wg.Add(1)
		go func() { defer wg.Done(); d.append(loginSession{id: "a", username: "a"}, "Unknown browser", batch, now) }()
	}
	wg.Wait()
	if len(d.report(loginSession{username: "a"})["events"].([]diagnosticRecord)) != 1 {
		t.Fatal("concurrent replay duplicated events")
	}
	d.close()
	if err := os.Chmod(d.path, 0644); err != nil {
		t.Fatal(err)
	}
	bad := newDiagnosticStore(d.path)
	defer bad.close()
	if !bad.disabled || bad.append(loginSession{id: "a", username: "a"}, "Unknown browser", batch, now) != 503 {
		t.Fatal("unsafe persistence accepted")
	}
	if bad.report(loginSession{username: "a"})["storage_ok"] != false {
		t.Fatal("storage failure hidden")
	}
}

func TestDiagnosticsRejectCraftedPrivateRecordsOnRestart(t *testing.T) {
	now := time.Now()
	base := diagnosticRecord{Account: "owner", Client: diagnosticFixture(now).Client, Build: "app-test.js", Browser: "Safari on macOS", Received: now, diagnosticEvent: diagnosticFixture(now).Events[0]}
	mutations := []func(*diagnosticRecord){
		func(r *diagnosticRecord) { r.Reason = "SECRET" }, func(r *diagnosticRecord) { r.Route = "SECRET" }, func(r *diagnosticRecord) { r.Build = "SECRET" }, func(r *diagnosticRecord) { r.Browser = "SECRET" }, func(r *diagnosticRecord) { r.Client = "SECRET" }, func(r *diagnosticRecord) { r.Received = now.Add(48 * time.Hour) }, func(r *diagnosticRecord) { r.Account = strings.Repeat("x", 81) }, func(r *diagnosticRecord) { r.Profile = "SECRET" },
	}
	for i, mutate := range mutations {
		t.Run(fmt.Sprint(i), func(t *testing.T) {
			row := base
			mutate(&row)
			path := filepath.Join(t.TempDir(), "diagnostics.json")
			raw, _ := json.Marshal(map[string]any{"version": 1, "records": []diagnosticRecord{row}})
			if err := os.WriteFile(path, raw, 0600); err != nil {
				t.Fatal(err)
			}
			d := newDiagnosticStore(path)
			defer d.close()
			if !d.disabled || len(d.report(loginSession{username: "owner"})["events"].([]diagnosticRecord)) != 0 {
				t.Fatal("crafted records exported")
			}
			unchanged, _ := os.ReadFile(path)
			if string(unchanged) != string(raw) {
				t.Fatal("invalid original file overwritten")
			}
		})
	}
	rows := make([]diagnosticRecord, diagnosticAccountLimit+1)
	for i := range rows {
		rows[i] = base
		rows[i].Sequence = int64(i + 1)
	}
	if validDiagnosticRecords(rows, now) {
		t.Fatal("persisted per-account cap bypass")
	}
}

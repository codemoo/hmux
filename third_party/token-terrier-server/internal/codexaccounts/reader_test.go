package codexaccounts

import (
	"math"
	"os"
	"path/filepath"
	"testing"
	"time"

	"log/slog"
)

func readerFromString(t *testing.T, js string) *Reader {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "codex-lb-accounts.json")
	if err := os.WriteFile(path, []byte(js), 0o600); err != nil {
		t.Fatalf("write temp file: %v", err)
	}
	r := NewReader(path, slog.Default())
	r.now = func() time.Time {
		return time.Date(2026, 7, 3, 13, 1, 0, 0, time.UTC)
	}
	return r
}

func TestReader_MapsAndNormalizes(t *testing.T) {
	js := `{"schemaVersion":1,"accountsUpdatedAt":"2026-07-03T13:00:00Z","accounts":[
      {"number":1,"accountId":"aid1","email":"a@x","alias":" Work ","status":" active ","fiveHourPct":10,"sevenDayPct":0.92,"resetAtPrimary":"2026-07-03T15:00:00Z","resetAtSecondary":"2026-07-07T02:00:00Z","totalTokens":123,"tokensPerHour":45.6,"lastRefreshAt":"2026-07-03T12:55:00Z"},
      {"number":2,"accountId":"aid2","email":"b@x","alias":"","status":"paused","fiveHourPct":null,"sevenDayPct":0}]}`

	accts, updated := readerFromString(t, js).Accounts()

	if updated == nil || *updated != "2026-07-03T13:00:00.000Z" {
		t.Fatalf("expected accountsUpdatedAt passthrough, got %v", updated)
	}
	if len(accts) != 2 {
		t.Fatalf("expected 2 accounts, got %d", len(accts))
	}
	if accts[0].Status != "ok" { // active -> ok
		t.Errorf("expected status ok, got %q", accts[0].Status)
	}
	if accts[0].Email != "Work" { // display label: alias preferred
		t.Errorf("expected display label Work, got %q", accts[0].Email)
	}
	if accts[0].DisplayName != "Work" || accts[1].DisplayName != "Account 2" {
		t.Fatal("HMux names must use the codex-lb alias or numbered fallback")
	}
	if accts[0].FiveHour == nil || math.Abs(accts[0].FiveHour.UsedPct-1) > 1e-9 {
		t.Errorf("expected FiveHour.UsedPct clamped to 1, got %+v", accts[0].FiveHour)
	}
	if accts[0].FiveHour.ResetsAt == nil || *accts[0].FiveHour.ResetsAt != "2026-07-03T15:00:00.000Z" {
		t.Errorf("expected normalized reset, got %+v", accts[0].FiveHour.ResetsAt)
	}
	if accts[0].LastRefreshAt == nil || *accts[0].LastRefreshAt != "2026-07-03T12:55:00.000Z" {
		t.Errorf("expected normalized lastRefreshAt, got %v", accts[0].LastRefreshAt)
	}
	if accts[0].TokensPerHour == nil || *accts[0].TokensPerHour != 45.6 {
		t.Errorf("expected TokensPerHour 45.6, got %v", accts[0].TokensPerHour)
	}
	if accts[0].TotalTokens == nil || *accts[0].TotalTokens != 123 {
		t.Errorf("expected TotalTokens 123, got %v", accts[0].TotalTokens)
	}
	if accts[1].Status != "paused" { // pass-through unknown status
		t.Errorf("expected status paused pass-through, got %q", accts[1].Status)
	}
	if accts[1].FiveHour != nil {
		t.Errorf("expected nil FiveHour for null window, got %+v", accts[1].FiveHour)
	}
}

func TestHMuxDisplayNameNeverFallsBackToEmailOrDisplayName(t *testing.T) {
	accounts, _, err := parseAccounts([]byte(`{"schemaVersion":1,"accounts":[
		{"number":1,"alias":"","displayName":"Other display name","email":"private@example.test"},
		{"number":2,"alias":"Owner alias","displayName":"Ignored","email":"private@example.test"}
	]}`))
	if err != nil || len(accounts) != 2 {
		t.Fatalf("parse aliases: %v", err)
	}
	if accounts[0].DisplayName != "Account 1" || accounts[1].DisplayName != "Owner alias" {
		t.Fatal("account aliases were replaced by another identity field")
	}
}

func TestReader_FallsBackToFileMtimeWhenUpdatedAtMissing(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "codex-lb-accounts.json")
	js := `{"schemaVersion":1,"accounts":[{"number":1,"accountId":"aid1","email":"a@x","status":"active","fiveHourPct":0.1}]}`
	if err := os.WriteFile(path, []byte(js), 0o600); err != nil {
		t.Fatalf("write temp file: %v", err)
	}
	mtime := time.Date(2026, 7, 3, 13, 0, 0, 0, time.UTC)
	_ = os.Chtimes(path, mtime, mtime)

	r := NewReader(path, slog.Default())
	r.now = func() time.Time { return mtime.Add(time.Minute) }
	_, updated := r.Accounts()
	if updated == nil || *updated != "2026-07-03T13:00:00.000Z" {
		t.Fatalf("expected mtime fallback, got %v", updated)
	}
}

func TestReader_MalformedRewriteKeepsLastGood(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "codex-lb-accounts.json")
	good := `{"schemaVersion":1,"accountsUpdatedAt":"2026-07-03T13:00:00Z","accounts":[{"number":1,"accountId":"aid1","email":"a@x","status":"active","fiveHourPct":0.1}]}`
	if err := os.WriteFile(path, []byte(good), 0o600); err != nil {
		t.Fatalf("write good file: %v", err)
	}
	r := NewReader(path, slog.Default())
	r.checkEvery = 0
	base := time.Date(2026, 7, 3, 13, 0, 0, 0, time.UTC)
	now := base
	r.now = func() time.Time { return now }
	accts, updated := r.Accounts()
	if len(accts) != 1 || updated == nil {
		t.Fatalf("first read = (%+v,%v), want 1 account + updated", accts, updated)
	}

	future := base.Add(2 * time.Second)
	if err := os.WriteFile(path, []byte(`not json`), 0o600); err != nil {
		t.Fatalf("write malformed file: %v", err)
	}
	_ = os.Chtimes(path, future, future)
	now = base.Add(10 * time.Minute)
	accts, updated2 := r.Accounts()
	if len(accts) != 1 || accts[0].Email != "a@x" {
		t.Fatalf("malformed rewrite should keep last-good accounts, got %+v", accts)
	}
	if updated2 == nil || *updated2 != *updated {
		t.Fatalf("malformed rewrite should keep last-good updated_at, got %v want %v", updated2, updated)
	}
	status := r.Status()
	if status.State != ReaderStateParseError || !status.UsingLastGood || status.LastGoodExpired {
		t.Fatalf("malformed replacement status = %+v", status)
	}
	if status.LastErrorAt == nil || status.LastErrorKind != "parse" || status.AgeSeconds == nil || *status.AgeSeconds != 600 {
		t.Fatalf("malformed replacement diagnostics = %+v", status)
	}

	now = base.Add(defaultMaxLastGoodAge + time.Second)
	accts, updated2 = r.Accounts()
	if accts != nil || updated2 != nil {
		t.Fatalf("expired malformed last-good should be omitted, got (%+v,%v)", accts, updated2)
	}
	status = r.Status()
	if status.State != ReaderStateParseError || status.UsingLastGood || !status.LastGoodExpired {
		t.Fatalf("expired malformed replacement status = %+v", status)
	}
}

func TestParseAccountsRequiresAccountsArray(t *testing.T) {
	for _, payload := range []string{
		`{"schemaVersion":1}`,
		`{"schemaVersion":1,"accounts":null}`,
		`{"schemaVersion":1,"accounts":{}}`,
	} {
		if _, _, err := parseAccounts([]byte(payload)); err == nil {
			t.Fatalf("parseAccounts(%s) should reject invalid accounts shape", payload)
		}
	}
}

func TestParseAccountsRejectsWrongSchemaAndMalformedJSON(t *testing.T) {
	for _, payload := range []string{
		`{"schemaVersion":2,"accounts":[]}`,
		`not json`,
	} {
		if _, _, err := parseAccounts([]byte(payload)); err == nil {
			t.Fatalf("parseAccounts(%s) should fail", payload)
		}
	}
}

func TestParseAccountsZeroAccountsIsNilNoError(t *testing.T) {
	accts, _, err := parseAccounts([]byte(`{"schemaVersion":1,"accounts":[]}`))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if accts != nil {
		t.Fatalf("zero accounts = %+v, want nil", accts)
	}
}

func TestParseAccountsRejectsNonPositiveAndDuplicateNumbers(t *testing.T) {
	for _, payload := range []string{
		`{"schemaVersion":1,"accounts":[{"number":0,"email":"zero"}]}`,
		`{"schemaVersion":1,"accounts":[{"number":-1,"email":"negative"}]}`,
		`{"schemaVersion":1,"accounts":[{"number":1,"email":"one"},{"number":1,"email":"duplicate"}]}`,
	} {
		if _, _, err := parseAccounts([]byte(payload)); err == nil {
			t.Fatalf("parseAccounts(%s) should reject invalid account identity", payload)
		}
	}
}

func TestParseAccountsNormalizesLabelsAndFinitePercentageRange(t *testing.T) {
	payload := `{"schemaVersion":1,"accounts":[
	  {"number":1,"email":"  ","alias":" ","displayName":" ","status":"active","fiveHourPct":-2,"sevenDayPct":3},
	  {"number":2,"email":"  named@example.com  ","status":"paused"}]}`
	accts, _, err := parseAccounts([]byte(payload))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if accts[0].Email != "Account 1" {
		t.Fatalf("empty display fields not normalized: %+v", accts[0])
	}
	if accts[1].Email != "named@example.com" {
		t.Fatalf("display field not trimmed: %+v", accts[1])
	}
	if accts[0].FiveHour == nil || accts[0].FiveHour.UsedPct != 0 {
		t.Fatalf("negative percentage should clamp to 0: %+v", accts[0].FiveHour)
	}
	if accts[0].SevenDay == nil || accts[0].SevenDay.UsedPct != 1 {
		t.Fatalf("percentage over 1 should clamp to 1: %+v", accts[0].SevenDay)
	}
	nan := math.NaN()
	if _, err := toWindow(&nan, nil); err == nil {
		t.Fatal("non-finite percentage should be rejected")
	}
}

func TestReaderMissingFileUsesBoundedLastGoodThenExpires(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "codex-lb-accounts.json")
	base := time.Date(2026, 7, 16, 3, 0, 0, 0, time.UTC)
	good := `{"schemaVersion":1,"accountsUpdatedAt":"2026-07-16T03:00:00Z","accounts":[{"number":1,"email":"a@x","status":"active","fiveHourPct":0.1}]}`
	if err := os.WriteFile(path, []byte(good), 0o600); err != nil {
		t.Fatal(err)
	}
	now := base
	r := NewReader(path, slog.Default())
	r.checkEvery = 0
	r.now = func() time.Time { return now }
	if accts, _ := r.Accounts(); len(accts) != 1 {
		t.Fatalf("first read len = %d, want 1", len(accts))
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}

	now = base.Add(10 * time.Minute)
	accts, updated := r.Accounts()
	if len(accts) != 1 || updated == nil {
		t.Fatalf("recent missing replacement should use last-good, got (%v,%v)", accts, updated)
	}
	status := r.Status()
	if status.State != ReaderStateMissing || !status.UsingLastGood || status.LastGoodExpired {
		t.Fatalf("missing status = %+v", status)
	}
	if status.LastErrorAt != nil || status.LastErrorKind != "" {
		t.Fatalf("optional missing file must not be classified as error: %+v", status)
	}

	now = base.Add(defaultMaxLastGoodAge + time.Second)
	accts, updated = r.Accounts()
	if accts != nil || updated != nil {
		t.Fatalf("expired missing last-good → (nil,nil), got (%v,%v)", accts, updated)
	}
	status = r.Status()
	if status.State != ReaderStateMissing || status.UsingLastGood || !status.LastGoodExpired {
		t.Fatalf("expired missing status = %+v", status)
	}
}

func TestReaderInitialMissingIsOptionalAbsence(t *testing.T) {
	r := NewReader(filepath.Join(t.TempDir(), "missing.json"), slog.Default())
	if accts, updated := r.Accounts(); accts != nil || updated != nil {
		t.Fatalf("initial missing → (nil,nil), got (%v,%v)", accts, updated)
	}
	status := r.Status()
	if status.State != ReaderStateMissing || status.UsingLastGood || status.LastGoodExpired {
		t.Fatalf("initial missing status = %+v", status)
	}
	if status.LastCheckedAt == nil || status.LastErrorAt != nil || status.LastErrorKind != "" {
		t.Fatalf("initial missing diagnostics = %+v", status)
	}
}

func TestReaderStatErrorIsDistinctFromMissing(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "codex-lb-accounts.json")
	if err := os.Symlink("codex-lb-accounts.json", path); err != nil {
		t.Fatal(err)
	}
	r := NewReader(path, slog.Default())
	r.checkEvery = 0
	if accts, updated := r.Accounts(); accts != nil || updated != nil {
		t.Fatalf("stat error without last-good should not return accounts: (%v,%v)", accts, updated)
	}
	status := r.Status()
	if status.State != ReaderStateStatError || status.UsingLastGood || status.LastGoodExpired {
		t.Fatalf("stat error status = %+v", status)
	}
	if status.LastErrorAt == nil || status.LastErrorKind != "stat" {
		t.Fatalf("stat error diagnostics = %+v", status)
	}
}

func TestNormalizeStatusAliases(t *testing.T) {
	cases := map[string]string{
		" active ":        "ok",
		"enabled":         "ok",
		"logged-in":       "ok",
		"disabled":        "paused",
		"inactive":        "paused",
		"auth required":   "reauth_required",
		"reauthrequired":  "reauth_required",
		"token-expired":   "reauth_required",
		"rateLimited":     "rate_limited",
		"":                "unavailable",
		"reauth_required": "reauth_required",
	}
	for input, want := range cases {
		if got := normalizeStatus(input); got != want {
			t.Fatalf("normalizeStatus(%q) = %q, want %q", input, got, want)
		}
	}
}

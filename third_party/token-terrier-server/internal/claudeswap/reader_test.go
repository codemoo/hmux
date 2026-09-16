package claudeswap

import (
	"math"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

const sampleTwoAccounts = `{
  "schemaVersion": 1,
  "activeAccountNumber": 2,
  "accounts": [
    {"number":1,"email":"a@b.com","active":false,"usageStatus":"ok",
     "usage":{"fiveHour":{"pct":7.0,"resetsAt":"2026-07-03T12:00:00.318548+00:00"},
              "sevenDay":{"pct":29.0,"resetsAt":"2026-07-04T18:00:00.000+00:00"}}},
    {"number":2,"email":"c@d.com","active":true,"usageStatus":"ok",
     "usage":{"fiveHour":{"pct":50.0,"resetsAt":"2026-07-03T12:00:00Z"},
              "sevenDay":{"pct":80.0,"resetsAt":"2026-07-04T18:00:00Z"}}}
  ]
}`

func TestParseAccountsConvertsPctAndNormalizesReset(t *testing.T) {
	accts, err := parseAccounts([]byte(sampleTwoAccounts))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if len(accts) != 2 {
		t.Fatalf("len = %d, want 2", len(accts))
	}
	a := accts[0]
	if a.Number != 1 || a.Email != "a@b.com" || a.Active || a.Status != "ok" {
		t.Fatalf("account0 identity wrong: %+v", a)
	}
	if a.FiveHour == nil || math.Abs(a.FiveHour.UsedPct-0.07) > 1e-9 {
		t.Fatalf("5h pct = %v, want 0.07", a.FiveHour)
	}
	if a.FiveHour.ResetsAt == nil || *a.FiveHour.ResetsAt != "2026-07-03T12:00:00.318Z" {
		t.Fatalf("5h reset = %v, want normalized millis Z", a.FiveHour.ResetsAt)
	}
	if accts[1].SevenDay == nil || math.Abs(accts[1].SevenDay.UsedPct-0.80) > 1e-9 {
		t.Fatalf("acct1 7d = %v, want 0.80", accts[1].SevenDay)
	}
}

func TestParseAccountsRejectsWrongSchema(t *testing.T) {
	if _, err := parseAccounts([]byte(`{"schemaVersion":2,"accounts":[]}`)); err == nil {
		t.Fatal("expected error for schemaVersion 2")
	}
}

func TestParseAccountsZeroAccountsIsNilNoError(t *testing.T) {
	accts, err := parseAccounts([]byte(`{"schemaVersion":1,"accounts":[]}`))
	if err != nil {
		t.Fatalf("err = %v, want nil", err)
	}
	if accts != nil {
		t.Fatalf("accts = %v, want nil", accts)
	}
}

func TestParseAccountsNonOKStatusHasNilWindows(t *testing.T) {
	data := `{"schemaVersion":1,"accounts":[
	  {"number":3,"email":"e@f.com","active":false,"usageStatus":"api_key","usage":null}]}`
	accts, err := parseAccounts([]byte(data))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if len(accts) != 1 || accts[0].Status != "api_key" {
		t.Fatalf("status wrong: %+v", accts)
	}
	if accts[0].FiveHour != nil || accts[0].SevenDay != nil {
		t.Fatalf("expected nil windows for api_key account")
	}
}

func TestParseAccountsMalformedJSONErrors(t *testing.T) {
	if _, err := parseAccounts([]byte(`not json`)); err == nil {
		t.Fatal("expected error for malformed json")
	}
}

func TestParseAccountsRequiresAccountsArray(t *testing.T) {
	for _, payload := range []string{
		`{"schemaVersion":1}`,
		`{"schemaVersion":1,"accounts":null}`,
		`{"schemaVersion":1,"accounts":{}}`,
	} {
		if _, err := parseAccounts([]byte(payload)); err == nil {
			t.Fatalf("parseAccounts(%s) should reject invalid accounts shape", payload)
		}
	}
}

func TestParseAccountsRejectsNonPositiveAndDuplicateNumbers(t *testing.T) {
	for _, payload := range []string{
		`{"schemaVersion":1,"accounts":[{"number":0,"email":"zero"}]}`,
		`{"schemaVersion":1,"accounts":[{"number":-1,"email":"negative"}]}`,
		`{"schemaVersion":1,"accounts":[{"number":1,"email":"one"},{"number":1,"email":"duplicate"}]}`,
	} {
		if _, err := parseAccounts([]byte(payload)); err == nil {
			t.Fatalf("parseAccounts(%s) should reject invalid account identity", payload)
		}
	}
}

func TestParseAccountsNormalizesLabelsAndFinitePercentageRange(t *testing.T) {
	payload := `{"schemaVersion":1,"accounts":[
	  {"number":1,"email":"  ","usageStatus":"  ","usage":{"fiveHour":{"pct":-20},"sevenDay":{"pct":250}}},
	  {"number":2,"email":"  named@example.com  ","usageStatus":" ok "}]}`
	accts, err := parseAccounts([]byte(payload))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if accts[0].Email != "Account 1" || accts[0].Status != "unavailable" {
		t.Fatalf("empty identity fields not normalized: %+v", accts[0])
	}
	if accts[1].Email != "named@example.com" || accts[1].Status != "ok" {
		t.Fatalf("identity fields not trimmed: %+v", accts[1])
	}
	if accts[0].FiveHour == nil || accts[0].FiveHour.UsedPct != 0 {
		t.Fatalf("negative percentage should clamp to 0: %+v", accts[0].FiveHour)
	}
	if accts[0].SevenDay == nil || accts[0].SevenDay.UsedPct != 1 {
		t.Fatalf("percentage over 100 should clamp to 1: %+v", accts[0].SevenDay)
	}
	if _, err := toWindow(&cswapWindow{Pct: math.NaN()}); err == nil {
		t.Fatal("non-finite percentage should be rejected")
	}
}

func TestParseAccountsUnparseableResetDropped(t *testing.T) {
	data := `{"schemaVersion":1,"accounts":[
	  {"number":1,"email":"a@b.com","active":true,"usageStatus":"ok",
	   "usage":{"fiveHour":{"pct":10.0,"resetsAt":"garbage"}}}]}`
	accts, err := parseAccounts([]byte(data))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if accts[0].FiveHour == nil {
		t.Fatal("window should exist even if reset unparseable")
	}
	if accts[0].FiveHour.ResetsAt != nil {
		t.Fatalf("unparseable reset should be nil, got %v", *accts[0].FiveHour.ResetsAt)
	}
	if accts[0].SevenDay != nil {
		t.Fatal("absent sevenDay should be nil")
	}
}

func TestReaderReadsFileAndReportsUpdatedAt(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	if err := os.WriteFile(path, []byte(sampleTwoAccounts), 0o600); err != nil {
		t.Fatal(err)
	}
	r := NewReader(path, nil)
	accts, updated := r.Accounts()
	if len(accts) != 2 {
		t.Fatalf("len = %d, want 2", len(accts))
	}
	if updated == nil || *updated == "" {
		t.Fatal("expected non-nil accounts_updated_at from file mtime")
	}
}

func TestReaderMissingFileReturnsNil(t *testing.T) {
	r := NewReader(filepath.Join(t.TempDir(), "nope.json"), nil)
	accts, updated := r.Accounts()
	if accts != nil || updated != nil {
		t.Fatalf("missing file → (nil,nil), got (%v,%v)", accts, updated)
	}
	status := r.Status()
	if status.State != ReaderStateMissing || status.UsingLastGood || status.LastGoodExpired {
		t.Fatalf("initial missing status = %+v", status)
	}
	if status.LastCheckedAt == nil || status.LastErrorAt != nil || status.LastErrorKind != "" {
		t.Fatalf("initial missing diagnostics = %+v", status)
	}
}

func TestReaderReparsesOnMtimeChange(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	if err := os.WriteFile(path, []byte(`{"schemaVersion":1,"accounts":[
	  {"number":1,"email":"a@b.com","active":true,"usageStatus":"ok","usage":null}]}`), 0o600); err != nil {
		t.Fatal(err)
	}
	r := NewReader(path, nil)
	r.checkEvery = 0 // disable throttle for the test
	if accts, _ := r.Accounts(); len(accts) != 1 {
		t.Fatalf("first read len = %d, want 1", len(accts))
	}
	// Rewrite with two accounts and a newer mtime.
	future := time.Now().Add(2 * time.Second)
	if err := os.WriteFile(path, []byte(sampleTwoAccounts), 0o600); err != nil {
		t.Fatal(err)
	}
	_ = os.Chtimes(path, future, future)
	if accts, _ := r.Accounts(); len(accts) != 2 {
		t.Fatalf("after rewrite len = %d, want 2", len(accts))
	}
}

func TestReaderMissingFileUsesBoundedLastGoodThenExpires(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	if err := os.WriteFile(path, []byte(sampleTwoAccounts), 0o600); err != nil {
		t.Fatal(err)
	}
	base := time.Date(2026, 7, 16, 1, 0, 0, 0, time.UTC)
	if err := os.Chtimes(path, base, base); err != nil {
		t.Fatal(err)
	}
	now := base
	r := NewReader(path, nil)
	r.checkEvery = 0 // disable throttle for the test
	r.now = func() time.Time { return now }
	if accts, _ := r.Accounts(); len(accts) != 2 {
		t.Fatalf("first read len = %d, want 2", len(accts))
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	now = base.Add(10 * time.Minute)
	accts, updated := r.Accounts()
	if len(accts) != 2 || updated == nil {
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

func TestReaderMalformedRewriteKeepsLastGood(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	if err := os.WriteFile(path, []byte(sampleTwoAccounts), 0o600); err != nil {
		t.Fatal(err)
	}
	r := NewReader(path, nil)
	r.checkEvery = 0 // disable throttle for the test
	base := time.Date(2026, 7, 16, 2, 0, 0, 0, time.UTC)
	if err := os.Chtimes(path, base, base); err != nil {
		t.Fatal(err)
	}
	now := base
	r.now = func() time.Time { return now }
	accts, updated := r.Accounts()
	if len(accts) != 2 || updated == nil {
		t.Fatalf("first read = (%+v,%v), want 2 accounts + updated", accts, updated)
	}

	future := base.Add(2 * time.Second)
	if err := os.WriteFile(path, []byte(`not json`), 0o600); err != nil {
		t.Fatal(err)
	}
	_ = os.Chtimes(path, future, future)
	now = base.Add(10 * time.Minute)
	accts, updated2 := r.Accounts()
	if len(accts) != 2 {
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

func TestReaderStatErrorIsDistinctFromMissing(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	if err := os.Symlink("accounts.json", path); err != nil {
		t.Fatal(err)
	}
	r := NewReader(path, nil)
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

func TestReaderWrongSchemaReturnsNil(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	_ = os.WriteFile(path, []byte(`{"schemaVersion":9,"accounts":[]}`), 0o600)
	r := NewReader(path, nil)
	if accts, updated := r.Accounts(); accts != nil || updated != nil {
		t.Fatalf("wrong schema → (nil,nil), got (%v,%v)", accts, updated)
	}
}

type fakeActivity struct {
	mu    sync.Mutex
	stats map[int]ActivitySnapshot
}

func (f *fakeActivity) Snapshot(accountNumber int, now time.Time) (ActivitySnapshot, bool) {
	f.mu.Lock()
	defer f.mu.Unlock()
	stats, ok := f.stats[accountNumber]
	return stats, ok
}

func (f *fakeActivity) set(accountNumber int, stats ActivitySnapshot) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.stats == nil {
		f.stats = map[int]ActivitySnapshot{}
	}
	f.stats[accountNumber] = stats
}

func TestReaderOverlaysActivityWithoutReparse(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	if err := os.WriteFile(path, []byte(sampleTwoAccounts), 0o600); err != nil {
		t.Fatal(err)
	}
	activity := &fakeActivity{stats: map[int]ActivitySnapshot{
		1: {TokensPerHour: 1200, TotalTokens: 300},
	}}
	r := NewReader(path, nil)
	r.SetActivityProvider(activity)
	r.checkEvery = time.Hour

	accts, _ := r.Accounts()
	if len(accts) != 2 {
		t.Fatalf("len = %d, want 2", len(accts))
	}
	if accts[0].TokensPerHour == nil || *accts[0].TokensPerHour != 1200 {
		t.Fatalf("account 1 tokens/hour = %v, want 1200", accts[0].TokensPerHour)
	}
	if accts[0].TotalTokens == nil || *accts[0].TotalTokens != 300 {
		t.Fatalf("account 1 total = %v, want 300", accts[0].TotalTokens)
	}
	if accts[1].TokensPerHour != nil || accts[1].TotalTokens != nil {
		t.Fatalf("account 2 should have no overlay, got %+v", accts[1])
	}

	activity.set(1, ActivitySnapshot{TokensPerHour: 2400, TotalTokens: 700})
	accts, _ = r.Accounts()
	if accts[0].TokensPerHour == nil || *accts[0].TokensPerHour != 2400 {
		t.Fatalf("throttled read should still refresh overlay, got %v", accts[0].TokensPerHour)
	}
	if accts[0].TotalTokens == nil || *accts[0].TotalTokens != 700 {
		t.Fatalf("throttled read should still refresh total, got %v", accts[0].TotalTokens)
	}
}

func TestReaderActiveAccountNumber(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "accounts.json")
	if err := os.WriteFile(path, []byte(sampleTwoAccounts), 0o600); err != nil {
		t.Fatal(err)
	}
	r := NewReader(path, nil)
	if got := r.ActiveAccountNumber(); got != 2 {
		t.Fatalf("active account = %d, want 2", got)
	}
}

package claudeswap

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestCommandReaderUsesExactReadOnlyArgvAndPreservesFreshness(t *testing.T) {
	dir := t.TempDir()
	argsPath := filepath.Join(dir, "args")
	t.Setenv("CSWAP_ARGS_LOG", argsPath)
	fetched := time.Now().Add(-6 * time.Minute).UTC().Format(time.RFC3339)
	lastGood := time.Now().Add(-2 * time.Minute).UTC().Format(time.RFC3339)
	payload := `{"schemaVersion":1,"activeAccountNumber":1,"accounts":[` +
		`{"number":1,"email":"one@example.test","alias":"Primary","active":true,"usageStatus":"ok",` +
		`"usage":{"fiveHour":{"pct":25}},"usageFetchedAt":"` + fetched + `","usageAgeSeconds":360},` +
		`{"number":2,"email":"two@example.test","active":false,"usageStatus":"keychain_unavailable",` +
		`"usage":null,"lastGoodUsage":{"sevenDay":{"pct":50}},"lastGoodFetchedAt":"` + lastGood + `","lastGoodAgeSeconds":120}]}`
	executable := writeCommandFixture(t, dir, "printf '%s\\n' \"$@\" > \"$CSWAP_ARGS_LOG\"\nprintf '%s' '"+payload+"'\nprintf 'private diagnostic' >&2")
	r := NewCommandReader(dir, nil)
	r.lookup = func(string) (string, error) { return executable, nil }
	if err := r.Refresh(context.Background()); err != nil {
		t.Fatal(err)
	}
	args, err := os.ReadFile(argsPath)
	if err != nil || string(args) != "list\n--json\n" {
		t.Fatalf("argv=%q err=%v", args, err)
	}
	accounts, updated := r.Accounts()
	if len(accounts) != 2 || accounts[0].Status != "ok" || accounts[0].DisplayName != "Primary" {
		t.Fatalf("freshness/alias not preserved: %+v", accounts)
	}
	if accounts[1].Status != "keychain_unavailable" || accounts[1].SevenDay == nil || accounts[1].LastRefreshAt == nil {
		t.Fatalf("last-good relabeled or lost: %+v", accounts[1])
	}
	if updated == nil || *updated != *accounts[1].LastRefreshAt {
		t.Fatalf("updated=%v account=%v", updated, accounts[1].LastRefreshAt)
	}
}

func TestCommandReaderBoundsTimeoutAndStdout(t *testing.T) {
	for name, body := range map[string]string{
		"timeout": "sleep 1",
		"output":  "printf 'abcdefghijklmnopqrstuvwxyz'",
	} {
		t.Run(name, func(t *testing.T) {
			dir := t.TempDir()
			executable := writeCommandFixture(t, dir, body)
			r := NewCommandReader(dir, nil)
			r.lookup = func(string) (string, error) { return executable, nil }
			r.timeout = 30 * time.Millisecond
			r.maxOutput = 8
			err := r.Refresh(context.Background())
			if err == nil {
				t.Fatal("unbounded command succeeded")
			}
			if strings.Contains(err.Error(), "diagnostic") {
				t.Fatalf("stderr leaked: %v", err)
			}
		})
	}
}

func TestCommandReaderFallsBackToUserLocalBin(t *testing.T) {
	home := t.TempDir()
	want := filepath.Join(home, ".local", "bin", "cswap")
	r := NewCommandReader(home, nil)
	r.lookup = func(name string) (string, error) {
		if name == "cswap" {
			return "", errors.New("missing")
		}
		if name == want {
			return want, nil
		}
		return "", errors.New("unexpected")
	}
	if got, err := r.resolve(); err != nil || got != want {
		t.Fatalf("resolve=%q err=%v", got, err)
	}
}

func TestCommandParserDoesNotTreatUnknownFreshnessAsCurrent(t *testing.T) {
	rows, updated, err := parseCommandAccounts([]byte(`{"schemaVersion":1,"activeAccountNumber":1,"accounts":[
		{"number":1,"email":"unknown@example.test","active":true,"usageStatus":"ok","usage":{"fiveHour":{"pct":20}}}
	]}`), time.Now())
	if err != nil || len(rows) != 1 {
		t.Fatalf("parse=%+v updated=%v err=%v", rows, updated, err)
	}
	if rows[0].Status != "unavailable" || rows[0].FiveHour != nil || updated != nil {
		t.Fatalf("unknown freshness presented as current: %+v", rows[0])
	}
}

func TestCommandParserHonorsCSwapOKThroughBoundedDisplayAge(t *testing.T) {
	now := time.Date(2026, 9, 22, 12, 0, 0, 0, time.UTC)
	tests := map[string]struct {
		age        time.Duration
		wantStatus string
		wantWindow bool
	}{
		"within_bound": {age: 29 * time.Minute, wantStatus: "ok", wantWindow: true},
		"expired":      {age: 31 * time.Minute, wantStatus: "unavailable", wantWindow: false},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			fetched := now.Add(-test.age).Format(time.RFC3339)
			payload := `{"schemaVersion":1,"activeAccountNumber":1,"accounts":[` +
				`{"number":1,"email":"bounded@example.test","active":true,"usageStatus":"ok",` +
				`"usage":{"sevenDay":{"pct":30}},"usageFetchedAt":"` + fetched + `"}]}`
			rows, _, err := parseCommandAccounts([]byte(payload), now)
			if err != nil || len(rows) != 1 {
				t.Fatalf("parse=%+v err=%v", rows, err)
			}
			if rows[0].Status != test.wantStatus || (rows[0].SevenDay != nil) != test.wantWindow {
				t.Fatalf("bounded status/window=%+v", rows[0])
			}
		})
	}
}

func writeCommandFixture(t *testing.T, dir, body string) string {
	t.Helper()
	path := filepath.Join(dir, "cswap")
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"+body+"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	return path
}

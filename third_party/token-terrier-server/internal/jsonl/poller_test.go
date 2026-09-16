package jsonl

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestParseClaudeSwapSessionAccountNumber(t *testing.T) {
	cases := []struct {
		name string
		want int
	}{
		{name: "1-a-at-b-com", want: 1},
		{name: "02-two-at-example-com", want: 2},
		{name: "no-number", want: 0},
		{name: "3", want: 0},
		{name: "3_projects", want: 0},
		{name: "0-zero", want: 0},
	}
	for _, tc := range cases {
		if got := parseClaudeSwapSessionAccountNumber(tc.name); got != tc.want {
			t.Fatalf("parse(%q) = %d, want %d", tc.name, got, tc.want)
		}
	}
}

func TestDiscoverClaudeSwapProjectRoots(t *testing.T) {
	root := t.TempDir()
	mustMkdir(t, filepath.Join(root, "1-a-at-b-com", "projects"))
	mustMkdir(t, filepath.Join(root, "02-two-at-example-com", "projects"))
	mustMkdir(t, filepath.Join(root, "3-no-projects"))
	mustMkdir(t, filepath.Join(root, "bad-name", "projects"))
	if err := os.WriteFile(filepath.Join(root, "4-file"), []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}

	roots := discoverClaudeSwapProjectRoots(root)
	if len(roots) != 2 {
		t.Fatalf("len = %d, want 2: %+v", len(roots), roots)
	}
	if roots[0].claudeAccountNumber != 1 || roots[0].path != filepath.Join(root, "1-a-at-b-com", "projects") {
		t.Fatalf("root 0 = %+v", roots[0])
	}
	if roots[1].claudeAccountNumber != 2 || roots[1].path != filepath.Join(root, "02-two-at-example-com", "projects") {
		t.Fatalf("root 1 = %+v", roots[1])
	}
}

func TestParseAndEmitAttachesClaudeAccountNumber(t *testing.T) {
	var events []TokenEvent
	p := &Poller{emit: func(ev TokenEvent) {
		events = append(events, ev)
	}}
	line := []byte(`{"type":"assistant","timestamp":"2026-07-04T01:02:03.000Z","sessionId":"s1","message":{"model":"claude-sonnet","usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":5}}}` + "\n")

	consumed := p.parseAndEmit(pollRoot{
		provider:            wire.ProviderClaude,
		claudeAccountNumber: 7,
	}, filepath.Join("projects", "session.jsonl"), line)

	if consumed != len(line) {
		t.Fatalf("consumed = %d, want %d", consumed, len(line))
	}
	if len(events) != 1 {
		t.Fatalf("events = %d, want 1", len(events))
	}
	if events[0].AccountNumber != 7 {
		t.Fatalf("account number = %d, want 7", events[0].AccountNumber)
	}
	if events[0].Tokens != 35 {
		t.Fatalf("tokens = %d, want 35", events[0].Tokens)
	}
	wantTS := time.Date(2026, 7, 4, 1, 2, 3, 0, time.UTC)
	if !events[0].Timestamp.Equal(wantTS) {
		t.Fatalf("timestamp = %s, want %s", events[0].Timestamp, wantTS)
	}
}

func mustMkdir(t *testing.T, path string) {
	t.Helper()
	if err := os.MkdirAll(path, 0o700); err != nil {
		t.Fatal(err)
	}
}

package jsonl

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestPollerReadsNewSessionFromBeginningAfterBootstrap(t *testing.T) {
	root := t.TempDir()
	claudeRoot := filepath.Join(root, "claude")
	codexRoot := filepath.Join(root, "codex")
	if err := os.MkdirAll(claudeRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(codexRoot, 0o700); err != nil {
		t.Fatal(err)
	}

	var events []TokenEvent
	poller := NewPoller(func(event TokenEvent) {
		events = append(events, event)
	}, nil)
	poller.ClaudeRoot = claudeRoot
	poller.CodexRoot = codexRoot
	poller.DisableClaudeSwapSessions = true
	poller.bootstrapOffsets(context.Background())

	line := []byte(`{"type":"assistant","timestamp":"2026-07-16T00:00:00.000Z","sessionId":"new-session","message":{"model":"test","usage":{"input_tokens":25,"output_tokens":0}}}` + "\n")
	path := filepath.Join(claudeRoot, "new.jsonl")
	if err := os.WriteFile(path, line, 0o600); err != nil {
		t.Fatal(err)
	}
	poller.tickRoot(context.Background(), pollRoot{
		provider: wire.ProviderClaude,
		path:     claudeRoot,
	})

	if len(events) != 1 {
		t.Fatalf("events = %d, want 1", len(events))
	}
	if events[0].Tokens != 25 || events[0].SessionKey != "new-session" {
		t.Fatalf("event = %+v", events[0])
	}
}

func TestJSONLDiscoveryAndTailRejectSymlinksAndFIFOWithoutBlocking(t *testing.T) {
	root := t.TempDir()
	regular := filepath.Join(root, "regular.jsonl")
	if err := os.WriteFile(regular, []byte("{}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(root, "linked.jsonl")
	if err := os.Symlink(regular, link); err != nil {
		t.Fatal(err)
	}
	fifo := filepath.Join(root, "blocked.jsonl")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	poller := NewPoller(func(TokenEvent) {}, nil)
	listing, err := poller.listJSONL(context.Background(), root)
	if err != nil {
		t.Fatal(err)
	}
	if len(listing) != 1 || listing[regular] != 3 {
		t.Fatalf("unsafe JSONL entries were listed: %#v", listing)
	}
	started := time.Now()
	if _, err := poller.tailFrom(context.Background(), fifo, 0); err == nil {
		t.Fatal("FIFO tail was accepted")
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("FIFO tail blocked for %v", elapsed)
	}
	if _, err := poller.tailFrom(context.Background(), link, 0); err == nil {
		t.Fatal("symlink tail was accepted")
	}
}

func TestBootstrapBackfillsOnlyCurrentLocalDayFromExistingFiles(t *testing.T) {
	root := t.TempDir()
	claudeRoot := filepath.Join(root, "claude")
	codexRoot := filepath.Join(root, "codex")
	if err := os.MkdirAll(claudeRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(codexRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	now := time.Now().In(time.Local)
	dayStart := time.Date(now.Year(), now.Month(), now.Day(), 0, 0, 0, 0, now.Location())
	line := func(timestamp, session string) string {
		return `{"type":"assistant","timestamp":"` + timestamp + `","sessionId":"` + session + `","message":{"model":"test","usage":{"input_tokens":25,"output_tokens":0}}}` + "\n"
	}
	path := filepath.Join(claudeRoot, "existing.jsonl")
	body := line(dayStart.Add(-time.Hour).UTC().Format(time.RFC3339Nano), "yesterday") +
		line(dayStart.Add(time.Hour).UTC().Format(time.RFC3339Nano), "today")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}

	var events []TokenEvent
	poller := NewPoller(func(event TokenEvent) { events = append(events, event) }, nil)
	poller.ClaudeRoot = claudeRoot
	poller.CodexRoot = codexRoot
	poller.DisableClaudeSwapSessions = true
	poller.bootstrapOffsets(context.Background())

	if len(events) != 1 || events[0].SessionKey != "today" {
		t.Fatalf("bootstrap events = %+v, want only current-day event", events)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if got := poller.offsets[path]; got != info.Size() {
		t.Fatalf("bootstrap offset = %d, want EOF %d", got, info.Size())
	}
}

func TestTailReadAndLineParsingAreBounded(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, "large.jsonl")
	if err := os.WriteFile(path, bytes.Repeat([]byte("x"), maxTailReadBytes*2), 0o600); err != nil {
		t.Fatal(err)
	}
	poller := NewPoller(func(TokenEvent) {}, nil)
	chunk, err := poller.tailFrom(context.Background(), path, 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(chunk) != maxTailReadBytes {
		t.Fatalf("tail chunk = %d bytes, want bounded %d", len(chunk), maxTailReadBytes)
	}

	var events []TokenEvent
	poller.emit = func(event TokenEvent) { events = append(events, event) }
	valid := []byte(`{"type":"assistant","timestamp":"2026-07-16T00:00:00.000Z","sessionId":"valid","message":{"model":"test","usage":{"input_tokens":25,"output_tokens":0}}}`)
	buffer := make([]byte, 0, maxJSONLLineBytes+2+len(valid))
	buffer = append(buffer, bytes.Repeat([]byte("z"), maxJSONLLineBytes+1)...)
	buffer = append(buffer, '\n')
	buffer = append(buffer, valid...)
	buffer = append(buffer, '\n')
	consumed := poller.parseAndEmit(pollRoot{provider: wire.ProviderClaude}, path, buffer)
	if consumed != len(buffer) {
		t.Fatalf("consumed = %d, want %d", consumed, len(buffer))
	}
	if len(events) != 1 || events[0].SessionKey != "valid" {
		t.Fatalf("oversized line was not skipped cleanly: %+v", events)
	}
}

func TestOversizedPartialLineDiscardsUntilNewline(t *testing.T) {
	poller := NewPoller(func(TokenEvent) {
		t.Fatal("continuation of oversized line must not emit")
	}, nil)
	path := "/tmp/oversized-partial-test.jsonl"
	first := bytes.Repeat([]byte("x"), maxJSONLLineBytes+1)
	if consumed := poller.parseAndEmit(pollRoot{provider: wire.ProviderClaude}, path, first); consumed != len(first) {
		t.Fatalf("first consumed = %d, want %d", consumed, len(first))
	}
	continuation := []byte(`{"type":"assistant"}` + "\n")
	if consumed := poller.parseAndEmit(pollRoot{provider: wire.ProviderClaude}, path, continuation); consumed != len(continuation) {
		t.Fatalf("continuation consumed = %d, want %d", consumed, len(continuation))
	}
}

func TestPollerStatusDistinguishesSuccessfulAndMissingRoots(t *testing.T) {
	root := t.TempDir()
	claudeRoot := filepath.Join(root, "claude")
	if err := os.MkdirAll(claudeRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	poller := NewPoller(nil, nil)
	poller.ClaudeRoot = claudeRoot
	poller.CodexRoot = filepath.Join(root, "missing-codex")
	poller.DisableClaudeSwapSessions = true
	poller.tick(context.Background())

	claude := poller.Status(wire.ProviderClaude)
	if !claude.Observed || claude.State != "ok" || claude.LastScanAt == nil || claude.LastSuccessAt == nil || claude.LastErrorAt != nil {
		t.Fatalf("claude status = %+v", claude)
	}
	codex := poller.Status(wire.ProviderCodex)
	if !codex.Observed || codex.State != "missing" || codex.LastScanAt == nil || codex.LastErrorAt == nil || codex.LastErrorKind != "missing" {
		t.Fatalf("codex status = %+v", codex)
	}
}

func TestHotDirectoryPollAvoidsColdTreeUntilLowFrequencyReconcile(t *testing.T) {
	root := t.TempDir()
	claudeRoot := filepath.Join(root, "claude")
	hotDir := filepath.Join(claudeRoot, "hot-project")
	coldDir := filepath.Join(claudeRoot, "cold-project")
	for _, dir := range []string{hotDir, coldDir} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	old := time.Now().Add(-48 * time.Hour)
	coldBaseline := filepath.Join(coldDir, "old.jsonl")
	if err := os.WriteFile(coldBaseline, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(coldBaseline, old, old); err != nil {
		t.Fatal(err)
	}
	hotBaseline := filepath.Join(hotDir, "current.jsonl")
	if err := os.WriteFile(hotBaseline, nil, 0o600); err != nil {
		t.Fatal(err)
	}

	var events []TokenEvent
	poller := NewPoller(func(event TokenEvent) { events = append(events, event) }, nil)
	poller.ClaudeRoot = claudeRoot
	poller.CodexRoot = filepath.Join(root, "codex")
	poller.DisableClaudeSwapSessions = true
	poller.ReconcileInterval = time.Hour
	poller.bootstrapOffsets(context.Background())

	line := func(session string) []byte {
		return []byte(`{"type":"assistant","timestamp":"2026-07-24T00:00:00Z","sessionId":"` + session + `","message":{"model":"test","usage":{"input_tokens":10}}}` + "\n")
	}
	if err := os.WriteFile(filepath.Join(hotDir, "new-hot.jsonl"), line("hot"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(coldDir, "new-cold.jsonl"), line("cold"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := poller.tickRoot(context.Background(), pollRoot{provider: wire.ProviderClaude, path: claudeRoot}); err != nil {
		t.Fatal(err)
	}
	if len(events) != 1 || events[0].SessionKey != "hot" {
		t.Fatalf("fast poll events = %+v, want only hot directory", events)
	}

	poller.mu.Lock()
	poller.lastReconcile[claudeRoot] = time.Now().Add(-2 * time.Hour)
	poller.mu.Unlock()
	if err := poller.tickRoot(context.Background(), pollRoot{provider: wire.ProviderClaude, path: claudeRoot}); err != nil {
		t.Fatal(err)
	}
	if len(events) != 2 || events[1].SessionKey != "cold" {
		t.Fatalf("reconcile events = %+v, want newly active cold directory", events)
	}
}

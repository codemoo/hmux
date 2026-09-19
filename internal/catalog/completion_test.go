package catalog

import (
	"context"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

func completionFixture(t *testing.T, initial ...string) (*CompletionTracker, model.Catalog, string, string) {
	t.Helper()
	home := t.TempDir()
	root := filepath.Join(home, ".codex", "sessions")
	path := bindingRollout(t, root, "completion-main", `"cli"`)
	appendCompletionLines(t, path, initial...)

	processes := filepath.Join(t.TempDir(), "ps")
	if err := os.WriteFile(processes, []byte("#!/bin/sh\nprintf '%s\\n' '10 1 Ss 0.0 zsh' '20 10 S+ 0.0 codex'\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	lsofData := filepath.Join(t.TempDir(), "lsof-data")
	if err := os.WriteFile(lsofData, []byte("p20\nn"+path+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	lsof := filepath.Join(t.TempDir(), "lsof")
	if err := os.WriteFile(lsof, []byte("#!/bin/sh\ncat \"$HMUX_COMPLETION_LSOF\"\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HMUX_COMPLETION_LSOF", lsofData)
	tracker := &CompletionTracker{inspector: systemProcessInspector{HomeDir: home, PSPath: processes, LsofPath: lsof}}
	value := model.Catalog{ProtocolVersion: model.ProtocolVersion, Sessions: []model.Session{{
		ID: "$1", CreatedAt: 42, PanePID: 10,
	}}}
	return tracker, value, path, lsofData
}

func appendCompletionLines(t *testing.T, path string, lines ...string) {
	t.Helper()
	if len(lines) == 0 {
		return
	}
	file, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	for _, line := range lines {
		if _, err := file.WriteString(line + "\n"); err != nil {
			t.Fatal(err)
		}
	}
}

func observeCompletions(t *testing.T, tracker *CompletionTracker, value model.Catalog) []TaskCompletion {
	t.Helper()
	result, err := tracker.Observe(context.Background(), value)
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func TestCompletionTrackerBaselinesRunningTurnAndEmitsOnce(t *testing.T) {
	tracker, value, path, _ := completionFixture(t,
		`{"timestamp":"2026-09-20T01:02:00Z","type":"event_msg","payload":{"type":"task_started"}}`,
	)
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("baseline replayed completion: %#v", got)
	}
	appendCompletionLines(t, path, `{"timestamp":"2026-09-20T01:02:03.456Z","type":"event_msg","payload":{"type":"task_complete"}}`)
	got := observeCompletions(t, tracker, value)
	if len(got) != 1 || got[0].Session != (model.SessionIdentity{ID: "$1", CreatedAt: 42}) ||
		!got[0].CompletedAt.Equal(time.Date(2026, 9, 20, 1, 2, 3, 456000000, time.UTC)) ||
		!regexp.MustCompile(`^[0-9a-f]{64}$`).MatchString(got[0].EventID) {
		t.Fatalf("completion=%#v", got)
	}
	if repeated := observeCompletions(t, tracker, value); len(repeated) != 0 {
		t.Fatalf("completion repeated: %#v", repeated)
	}
}

func TestCompletionTrackerCatchesFastTurnBetweenObservations(t *testing.T) {
	tracker, value, path, _ := completionFixture(t)
	observeCompletions(t, tracker, value)
	appendCompletionLines(t, path,
		`{"timestamp":"2026-09-20T01:02:00Z","type":"task_started"}`,
		`{"timestamp":"2026-09-20T01:02:01Z","type":"task_complete"}`,
	)
	if got := observeCompletions(t, tracker, value); len(got) != 1 {
		t.Fatalf("fast completion count=%d values=%#v", len(got), got)
	}
}

func TestCompletionTrackerDoesNotReplayBaselineOrMissingTimestamp(t *testing.T) {
	tracker, value, path, _ := completionFixture(t,
		`{"timestamp":"2026-09-20T01:02:00Z","type":"task_started"}`,
		`{"timestamp":"2026-09-20T01:02:01Z","type":"task_complete"}`,
	)
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("historical completion replayed: %#v", got)
	}
	appendCompletionLines(t, path,
		`{"timestamp":"2026-09-20T01:03:00Z","type":"task_started"}`,
		`{"type":"task_complete"}`,
	)
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("timestamp-free completion emitted: %#v", got)
	}
}

func TestCompletionTrackerWaitsForCompleteJSONLine(t *testing.T) {
	tracker, value, path, _ := completionFixture(t,
		`{"timestamp":"2026-09-20T01:02:00Z","type":"task_started"}`,
	)
	observeCompletions(t, tracker, value)
	file, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	line := `{"timestamp":"2026-09-20T01:02:01Z","type":"task_complete"}`
	if _, err := file.WriteString(line); err != nil {
		t.Fatal(err)
	}
	file.Close()
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("partial line emitted: %#v", got)
	}
	file, err = os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := file.WriteString("\n"); err != nil {
		t.Fatal(err)
	}
	file.Close()
	if got := observeCompletions(t, tracker, value); len(got) != 1 {
		t.Fatalf("completed line count=%d values=%#v", len(got), got)
	}
}

func TestCompletionTrackerRebindAndReusedIdentityStartWithBaseline(t *testing.T) {
	tracker, value, path, lsofData := completionFixture(t,
		`{"timestamp":"2026-09-20T01:02:00Z","type":"task_started"}`,
	)
	observeCompletions(t, tracker, value)
	appendCompletionLines(t, path, `{"timestamp":"2026-09-20T01:02:01Z","type":"task_complete"}`)

	root := filepath.Dir(filepath.Dir(filepath.Dir(filepath.Dir(path))))
	other := bindingRollout(t, root, "replacement-main", `"cli"`)
	appendCompletionLines(t, other,
		`{"timestamp":"2026-09-20T01:03:00Z","type":"task_started"}`,
		`{"timestamp":"2026-09-20T01:03:01Z","type":"task_complete"}`,
	)
	if err := os.WriteFile(lsofData, []byte("p20\nn"+other+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("replacement record replayed: %#v", got)
	}

	value.Sessions[0].CreatedAt = 43
	appendCompletionLines(t, other,
		`{"timestamp":"2026-09-20T01:04:00Z","type":"task_started"}`,
		`{"timestamp":"2026-09-20T01:04:01Z","type":"task_complete"}`,
	)
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("reused tmux ID inherited cursor: %#v", got)
	}
	if len(tracker.cursors) != 1 {
		t.Fatalf("stale cursor count=%d", len(tracker.cursors))
	}
}

func TestCompletionTrackerAmbiguousBindingResetsBeforeRecovery(t *testing.T) {
	tracker, value, path, lsofData := completionFixture(t,
		`{"timestamp":"2026-09-20T01:02:00Z","type":"task_started"}`,
	)
	observeCompletions(t, tracker, value)
	otherRoot := filepath.Join(t.TempDir(), ".codex", "sessions")
	other := bindingRollout(t, otherRoot, "other-main", `"cli"`)
	if err := os.WriteFile(lsofData, []byte(strings.Join([]string{"p20", "n" + path, "n" + other, ""}, "\n")), 0o600); err != nil {
		t.Fatal(err)
	}
	appendCompletionLines(t, path, `{"timestamp":"2026-09-20T01:02:01Z","type":"task_complete"}`)
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("ambiguous completion emitted: %#v", got)
	}
	if err := os.WriteFile(lsofData, []byte("p20\nn"+path+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("completion during ambiguity replayed: %#v", got)
	}
}

func TestCompletionTrackerTruncationRebaselines(t *testing.T) {
	tracker, value, path, _ := completionFixture(t,
		`{"timestamp":"2026-09-20T01:02:00Z","type":"task_started","padding":"`+strings.Repeat("x", 512)+`"}`,
	)
	observeCompletions(t, tracker, value)
	header := `{"type":"session_meta","payload":{"id":"completion-main","source":"cli"}}` + "\n"
	replacement := header + strings.Join([]string{
		`{"timestamp":"2026-09-20T01:03:00Z","type":"task_started"}`,
		`{"timestamp":"2026-09-20T01:03:01Z","type":"task_complete"}`,
		"",
	}, "\n")
	if err := os.WriteFile(path, []byte(replacement), 0o600); err != nil {
		t.Fatal(err)
	}
	if got := observeCompletions(t, tracker, value); len(got) != 0 {
		t.Fatalf("truncated record replayed: %#v", got)
	}
}

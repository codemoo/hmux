package hermes

import (
	"context"
	"io"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestStatusClassifiesMissingSQLiteAndDatabase(t *testing.T) {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	poller := NewPoller(nil, logger)
	poller.SQLiteExecutable = filepath.Join(t.TempDir(), "sqlite3-does-not-exist")
	poller.DBPath = filepath.Join(t.TempDir(), "state.db")
	if rows := poller.tick(context.Background()); rows != -1 {
		t.Fatalf("rows = %d, want query failure", rows)
	}
	status := poller.Status()
	if !status.Observed || status.State != "error" || status.LastErrorKind != "sqlite3_missing" || status.LastScanAt == nil || status.LastErrorAt == nil {
		t.Fatalf("missing sqlite status = %+v", status)
	}

	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	poller.SQLiteExecutable = executable
	if rows := poller.tick(context.Background()); rows != -1 {
		t.Fatalf("rows = %d, want missing DB failure", rows)
	}
	if status := poller.Status(); status.LastErrorKind != "db_missing" {
		t.Fatalf("missing DB status = %+v", status)
	}
}

func TestSessionCreatedAndEndedBetweenPollsEmitsFinalDelta(t *testing.T) {
	sqlite, err := exec.LookPath("sqlite3")
	if err != nil {
		t.Skip("sqlite3 is not installed")
	}
	db := filepath.Join(t.TempDir(), "state.db")
	schema := `CREATE TABLE sessions (
        id TEXT, source TEXT, billing_provider TEXT, model TEXT,
        input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
        ended_at TEXT
    );`
	if output, err := exec.Command(sqlite, db, schema).CombinedOutput(); err != nil {
		t.Fatalf("create fixture DB: %v: %s", err, output)
	}
	var events []jsonl.TokenEvent
	poller := NewPoller(func(event jsonl.TokenEvent) { events = append(events, event) }, slog.New(slog.NewTextHandler(io.Discard, nil)))
	poller.SQLiteExecutable = sqlite
	poller.DBPath = db
	if rows := poller.tick(context.Background()); rows != 0 {
		t.Fatalf("bootstrap rows = %d, want 0", rows)
	}
	endedAt := time.Now().UTC().Format(time.RFC3339Nano)
	insert := `INSERT INTO sessions VALUES ('short','web','openai-codex','gpt',10,20,5,'` + endedAt + `');`
	if output, err := exec.Command(sqlite, db, insert).CombinedOutput(); err != nil {
		t.Fatalf("insert ended session: %v: %s", err, output)
	}
	if rows := poller.tick(context.Background()); rows != 0 {
		t.Fatalf("active rows = %d, want 0", rows)
	}
	if len(events) != 1 || events[0].Tokens != 35 || events[0].SessionKey != "hermes:short" {
		t.Fatalf("final delta events = %+v", events)
	}
	if rows := poller.tick(context.Background()); rows != 0 {
		t.Fatalf("repeat active rows = %d, want 0", rows)
	}
	if len(events) != 1 {
		t.Fatalf("overlapping cursor re-emitted ended session: %+v", events)
	}
}

func TestStatusClassifiesSchemaMismatch(t *testing.T) {
	sqlite, err := exec.LookPath("sqlite3")
	if err != nil {
		t.Skip("sqlite3 is not installed")
	}
	db := filepath.Join(t.TempDir(), "state.db")
	if output, err := exec.Command(sqlite, db, "CREATE TABLE unrelated (id TEXT);").CombinedOutput(); err != nil {
		t.Fatalf("create fixture DB: %v: %s", err, output)
	}
	poller := NewPoller(nil, slog.New(slog.NewTextHandler(io.Discard, nil)))
	poller.SQLiteExecutable = sqlite
	poller.DBPath = db
	if rows := poller.tick(context.Background()); rows != -1 {
		t.Fatalf("rows = %d, want schema failure", rows)
	}
	if status := poller.Status(); status.LastErrorKind != "schema_mismatch" {
		t.Fatalf("schema mismatch status = %+v", status)
	}
}

func TestCodexCLIIsDroppedOnlyWhileJSONLIsHealthy(t *testing.T) {
	var events []jsonl.TokenEvent
	poller := NewPoller(func(event jsonl.TokenEvent) { events = append(events, event) }, nil)
	row := sessionRow{
		ID:              "cli-session",
		Source:          "cli",
		BillingProvider: "openai-codex",
		Model:           "gpt",
		FreshTokens:     42,
	}

	poller.SetJSONLHealthy(func(wire.Provider) bool { return false })
	poller.mu.Lock()
	poller.dispatchLocked(row, 0)
	poller.mu.Unlock()
	if len(events) != 1 || events[0].Tokens != 42 {
		t.Fatalf("unhealthy JSONL must fall back to Hermes: %+v", events)
	}

	poller.SetJSONLHealthy(func(provider wire.Provider) bool { return provider == wire.ProviderCodex })
	poller.mu.Lock()
	poller.dispatchLocked(row, 0)
	poller.mu.Unlock()
	if len(events) != 1 {
		t.Fatalf("healthy JSONL overlap was not dropped: %+v", events)
	}
}

func TestClaudeCLIUsesSameHealthAwareSourceOwnership(t *testing.T) {
	var events []jsonl.TokenEvent
	poller := NewPoller(func(event jsonl.TokenEvent) { events = append(events, event) }, nil)
	row := sessionRow{
		ID:              "claude-cli-session",
		Source:          "cli",
		BillingProvider: "anthropic",
		Model:           "claude",
		FreshTokens:     50,
	}
	poller.SetJSONLHealthy(func(provider wire.Provider) bool { return provider == wire.ProviderClaude })
	poller.mu.Lock()
	poller.dispatchLocked(row, 0)
	poller.mu.Unlock()
	if len(events) != 0 {
		t.Fatalf("healthy Claude JSONL overlap was not dropped: %+v", events)
	}

	poller.SetJSONLHealthy(func(wire.Provider) bool { return false })
	poller.mu.Lock()
	poller.dispatchLocked(row, 0)
	poller.mu.Unlock()
	if len(events) != 1 || events[0].Provider != wire.ProviderClaude {
		t.Fatalf("unhealthy Claude JSONL must fall back to Hermes: %+v", events)
	}
}

// Package hermes polls ~/.hermes/state.db for completed/active
// LLM sessions and emits delta TokenEvents into the burn tracker.
//
// Mirrors Sources/TokenUsageCore/SQLite/HermesSQLiteWatcher.swift in spirit:
// keeps a per-session baseline of cumulative fresh tokens (input + output +
// reasoning), and on each scan emits whichever positive delta has appeared.
//
// Why we need it on top of jsonl.Poller: Hermes tracks ALL of the user's
// LLM API calls in one place — codex CLI sessions can live in both, but custom
// scripts and other tools that go through Hermes show up only here.
package hermes

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// Poller queries the local Hermes state database every PollInterval and emits
// TokenEvents for fresh-token deltas.
type Poller struct {
	DBPath           string
	SQLiteExecutable string
	PollInterval     time.Duration

	logger *slog.Logger
	emit   func(jsonl.TokenEvent)
	// jsonlHealthy confirms that the overlapping CLI stream is actually being
	// observed. A configured-but-missing JSONL root must not make Hermes drop
	// the only copy of an event.
	jsonlHealthy func(wire.Provider) bool

	mu          sync.Mutex
	baseline    map[string]int // sessionID → last fresh tokens observed
	seenEnded   map[string]time.Time
	queryCursor time.Time
	first       bool // first tick → only baseline, don't replay history
	status      Status
}

// Status is a privacy-safe capability summary for authenticated diagnostics.
type Status struct {
	Observed      bool
	State         string
	LastScanAt    *string
	LastSuccessAt *string
	LastErrorAt   *string
	LastErrorKind string
}

type capabilityError struct {
	kind string
	err  error
}

func (e *capabilityError) Error() string { return fmt.Sprintf("%s: %v", e.kind, e.err) }
func (e *capabilityError) Unwrap() error { return e.err }

// NewPoller builds a Hermes poller. Path defaults to ~/.hermes/state.db
// under the current user's home dir; override with TOKEN_USAGE_HERMES_DB.
func NewPoller(emit func(jsonl.TokenEvent), logger *slog.Logger) *Poller {
	if logger == nil {
		logger = slog.Default()
	}
	dbPath := strings.TrimSpace(os.Getenv("TOKEN_USAGE_HERMES_DB"))
	if dbPath == "" {
		home, _ := os.UserHomeDir()
		dbPath = filepath.Join(home, ".hermes", "state.db")
	}
	return &Poller{
		DBPath:           dbPath,
		SQLiteExecutable: "sqlite3",
		PollInterval:     30 * time.Second,
		logger:           logger,
		emit:             emit,
		baseline:         map[string]int{},
		seenEnded:        map[string]time.Time{},
		first:            true,
		status:           Status{State: "unobserved"},
	}
}

// SetEmitter wires the downstream state/hub callback before Run starts.
func (p *Poller) SetEmitter(emit func(jsonl.TokenEvent)) {
	p.mu.Lock()
	p.emit = emit
	p.mu.Unlock()
}

// SetJSONLHealthy installs the live overlap predicate used for CLI dedup.
// It must be wired before Run starts.
func (p *Poller) SetJSONLHealthy(healthy func(wire.Provider) bool) {
	p.mu.Lock()
	p.jsonlHealthy = healthy
	p.mu.Unlock()
}

// Run blocks until ctx cancels.
func (p *Poller) Run(ctx context.Context) {
	t := time.NewTicker(p.PollInterval)
	defer t.Stop()
	// Run one tick immediately to establish the baseline; without this the
	// first delta-emit happens 30 seconds after startup.
	if rows := p.tick(ctx); rows >= 0 {
		p.logger.Info("hermes poller bootstrap complete", "active_sessions", rows)
	}
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			p.tick(ctx)
		}
	}
}

// tick returns the number of active rows seen, or -1 on query failure.
func (p *Poller) tick(ctx context.Context) int {
	scanStartedAt := time.Now()
	p.mu.Lock()
	cursor := p.queryCursor
	p.mu.Unlock()
	rows, err := p.query(ctx, cursor)
	nowText := wire.FormatTime(time.Now())
	if err != nil {
		kind := "query_failed"
		var capability *capabilityError
		if errors.As(err, &capability) {
			kind = capability.kind
		}
		p.mu.Lock()
		previousKind := p.status.LastErrorKind
		p.status.Observed = true
		p.status.State = "error"
		p.status.LastScanAt = &nowText
		p.status.LastErrorAt = &nowText
		p.status.LastErrorKind = kind
		p.mu.Unlock()
		if previousKind != kind {
			p.logger.Warn("hermes capability unavailable", "kind", kind, "err", err)
		}
		return -1
	}

	p.mu.Lock()
	p.status.Observed = true
	p.status.State = "ok"
	p.status.LastScanAt = &nowText
	p.status.LastSuccessAt = &nowText
	p.status.LastErrorKind = ""
	// Use the query start, not completion, and overlap slightly. A session that
	// commits while sqlite3 is reading cannot fall between two cursors; the
	// seenEnded map removes overlap duplicates.
	p.queryCursor = scanStartedAt.Add(-2 * time.Second)
	wasFirst := p.first
	for id, seenAt := range p.seenEnded {
		if scanStartedAt.Sub(seenAt) > 10*time.Minute {
			delete(p.seenEnded, id)
		}
	}
	active := make(map[string]struct{}, len(rows))
	for _, row := range rows {
		// row.HasEnded → emit final delta and drop from baseline
		if row.HasEnded {
			if _, alreadySeen := p.seenEnded[row.ID]; alreadySeen {
				continue
			}
			if prev, ok := p.baseline[row.ID]; ok {
				p.dispatchLocked(row, prev)
				delete(p.baseline, row.ID)
			} else if !wasFirst {
				// A short-lived session can be created and end entirely
				// between two polls. It never had an active baseline, so its
				// final cumulative count is the only delta we will observe.
				p.dispatchLocked(row, 0)
			}
			p.seenEnded[row.ID] = scanStartedAt
			continue
		}

		active[row.ID] = struct{}{}
		if prev, ok := p.baseline[row.ID]; ok {
			p.dispatchLocked(row, prev)
			p.baseline[row.ID] = row.FreshTokens
		} else {
			p.baseline[row.ID] = row.FreshTokens
			if !wasFirst {
				// New active session discovered after startup — emit
				// its full token count once. (On the very first tick
				// we just record baselines; no historical replay.)
				p.dispatchLocked(row, 0)
			}
		}
	}

	// Drop baselines for sessions no longer active and not seen as ended
	// (e.g., DB row deleted). Without this the baseline map grows.
	for id := range p.baseline {
		if _, stillActive := active[id]; !stillActive {
			delete(p.baseline, id)
		}
	}
	p.first = false
	p.mu.Unlock()
	return len(active)
}

// Status returns the latest query/capability result without filesystem or
// process I/O. It is safe for authenticated diagnostics handlers.
func (p *Poller) Status() Status {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.status
}

// dispatchLocked computes the delta and emits one TokenEvent if positive.
// Caller holds p.mu.
//
// Dedup rule: skip rows whose source is already covered by another data
// stream the daemon ingests:
//
//   - `source='cli'` for either provider: normally also written to that
//     provider's JSONL. Skip it only while the matching JSONL source is
//     observed healthy; disabled/missing/error JSONL means Hermes is the only
//     usable activity source.
//   - `source='discord'`, `source='web'`, etc.: NOT in JSONL, must
//     emit so non-CLI codex usage is captured.
//
// Hermes exposes cumulative session rows while JSONL exposes request lines;
// there is no shared event identifier. Different SessionKey namespaces are
// not treated as dedup evidence.
func (p *Poller) dispatchLocked(row sessionRow, previous int) {
	delta := row.FreshTokens - previous
	if delta <= 0 {
		return
	}
	provider := mapProvider(row.BillingProvider)
	if provider == "" {
		return
	}
	if strings.EqualFold(row.Source, "cli") &&
		p.jsonlHealthy != nil && p.jsonlHealthy(provider) {
		// Confirmed covered by jsonl.Poller — skip.
		return
	}
	if p.emit == nil {
		return
	}
	p.emit(jsonl.TokenEvent{
		Provider:   provider,
		Timestamp:  time.Now(),
		Tokens:     delta,
		Model:      row.Model,
		SessionKey: "hermes:" + row.ID, // namespace sessions to avoid colliding with JSONL paths
	})
}

type sessionRow struct {
	ID              string
	Source          string // "cli", "discord", etc. — used for dedup against JSONL
	BillingProvider string
	Model           string
	FreshTokens     int
	HasEnded        bool
}

// query runs the Hermes session query with sqlite3 in read-only mode. Startup
// reads active rows only. Later scans add rows ended since an overlapping
// cursor, avoiding a full historical table scan and preserving sessions that
// start and finish entirely between polls.
func (p *Poller) query(ctx context.Context, endedSince time.Time) ([]sessionRow, error) {
	executable := strings.TrimSpace(p.SQLiteExecutable)
	if executable == "" {
		executable = "sqlite3"
	}
	resolved, err := exec.LookPath(executable)
	if err != nil {
		return nil, &capabilityError{kind: "sqlite3_missing", err: err}
	}
	info, err := os.Stat(p.DBPath)
	if err != nil {
		kind := "db_unreadable"
		if errors.Is(err, os.ErrNotExist) {
			kind = "db_missing"
		} else if errors.Is(err, os.ErrPermission) {
			kind = "db_permission"
		}
		return nil, &capabilityError{kind: kind, err: err}
	}
	if !info.Mode().IsRegular() {
		return nil, &capabilityError{kind: "db_not_regular", err: fmt.Errorf("not a regular file")}
	}
	// Use \x01 as separator so paths/values can contain pipe / tab safely.
	const sep = "\x01"
	where := "ended_at IS NULL"
	if !endedSince.IsZero() {
		cursor := strings.ReplaceAll(wire.FormatTime(endedSince), "'", "''")
		where += " OR julianday(ended_at) >= julianday('" + cursor + "')"
	}
	sql := `PRAGMA query_only=ON;SELECT id, source, billing_provider, model, input_tokens, output_tokens, reasoning_tokens, ended_at FROM sessions WHERE ` + where + ` ORDER BY id;`
	cmd := exec.CommandContext(ctx, resolved, "-readonly", "-separator", sep, p.DBPath, sql)
	out, err := cmd.Output()
	if err != nil {
		kind := "query_failed"
		message := strings.ToLower(err.Error())
		var exitErr *exec.ExitError
		if errors.As(err, &exitErr) {
			message += " " + strings.ToLower(string(exitErr.Stderr))
		}
		switch {
		case strings.Contains(message, "no such table"), strings.Contains(message, "no such column"):
			kind = "schema_mismatch"
		case strings.Contains(message, "permission"), strings.Contains(message, "readonly"):
			kind = "db_permission"
		}
		return nil, &capabilityError{kind: kind, err: err}
	}

	rows := make([]sessionRow, 0, 64)
	for _, line := range strings.Split(string(out), "\n") {
		line = strings.TrimRight(line, "\r")
		if line == "" {
			continue
		}
		parts := strings.Split(line, sep)
		if len(parts) < 8 {
			continue
		}
		input, _ := strconv.ParseInt(parts[4], 10, 64)
		output, _ := strconv.ParseInt(parts[5], 10, 64)
		reasoning, _ := strconv.ParseInt(parts[6], 10, 64)
		hasEnded := strings.TrimSpace(parts[7]) != ""
		rows = append(rows, sessionRow{
			ID:              parts[0],
			Source:          parts[1],
			BillingProvider: parts[2],
			Model:           parts[3],
			FreshTokens:     freshTokens(input, output, reasoning),
			HasEnded:        hasEnded,
		})
	}
	return rows, nil
}

func freshTokens(input, output, reasoning int64) int {
	var total int64
	for _, v := range []int64{input, output, reasoning} {
		if v < 0 {
			continue
		}
		total += v
	}
	if total < 0 || total > int64(int(^uint(0)>>1)) {
		return int(^uint(0) >> 1) // saturate at maxInt
	}
	return int(total)
}

// mapProvider matches Swift's lowercased substring rule: if billing_provider
// contains "codex" → codex; "anthropic" → claude; else unmapped.
func mapProvider(billingProvider string) wire.Provider {
	n := strings.ToLower(strings.TrimSpace(billingProvider))
	n = strings.ReplaceAll(n, "_", "-")
	if strings.Contains(n, "codex") {
		return wire.ProviderCodex
	}
	if strings.Contains(n, "anthropic") {
		return wire.ProviderClaude
	}
	return ""
}

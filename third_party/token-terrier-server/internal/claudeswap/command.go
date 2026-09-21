package claudeswap

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"os/exec"
	"path/filepath"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const (
	commandTimeout      = 10 * time.Second
	commandRefreshEvery = 60 * time.Second
	maximumCommandBytes = 8 * 1024 * 1024
)

// CommandReader runs the official, read-only `cswap list --json` command on a
// bounded cadence. It never switches accounts, logs in, starts services, or
// serializes command diagnostics. Concurrent callers share one in-flight run.
type CommandReader struct {
	home         string
	logger       *slog.Logger
	now          func() time.Time
	lookup       func(string) (string, error)
	timeout      time.Duration
	refreshEvery time.Duration
	maxOutput    int

	mu          sync.Mutex
	flight      *commandFlight
	lastAttempt time.Time
	lastGoodAt  time.Time
	accounts    []wire.AccountUsage
	updated     *string
}

type commandFlight struct {
	done chan struct{}
	err  error
}

func NewCommandReader(home string, logger *slog.Logger) *CommandReader {
	if logger == nil {
		logger = slog.Default()
	}
	return &CommandReader{
		home: home, logger: logger, now: time.Now, lookup: exec.LookPath,
		timeout: commandTimeout, refreshEvery: commandRefreshEvery, maxOutput: maximumCommandBytes,
	}
}

func (r *CommandReader) Refresh(ctx context.Context) error {
	if r == nil {
		return errors.New("claude-swap command reader is unavailable")
	}
	now := r.now()
	r.mu.Lock()
	if flight := r.flight; flight != nil {
		r.mu.Unlock()
		select {
		case <-flight.done:
			return flight.err
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	if !r.lastAttempt.IsZero() && now.Sub(r.lastAttempt) < r.refreshEvery {
		r.mu.Unlock()
		return nil
	}
	flight := &commandFlight{done: make(chan struct{})}
	r.flight = flight
	r.lastAttempt = now
	r.mu.Unlock()

	accounts, updated, err := r.run(ctx, now)
	r.mu.Lock()
	if err == nil {
		r.accounts = accounts
		r.updated = updated
		r.lastGoodAt = now
	}
	flight.err = err
	r.flight = nil
	close(flight.done)
	r.mu.Unlock()
	if err != nil {
		r.logger.Debug("claude-swap list failed", "kind", "command")
	}
	return err
}

func (r *CommandReader) Accounts() ([]wire.AccountUsage, *string) {
	if r == nil {
		return nil, nil
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.lastGoodAt.IsZero() || r.now().Sub(r.lastGoodAt) > defaultMaxLastGoodAge {
		return nil, nil
	}
	accounts := append([]wire.AccountUsage(nil), r.accounts...)
	for i := range accounts {
		observed := time.Time{}
		if accounts[i].LastRefreshAt != nil {
			observed, _ = time.Parse(time.RFC3339Nano, *accounts[i].LastRefreshAt)
		}
		if observed.IsZero() || r.now().Sub(observed) >= defaultMaxLastGoodAge {
			accounts[i].FiveHour, accounts[i].SevenDay = nil, nil
			if accounts[i].Status == "ok" {
				accounts[i].Status = "unavailable"
			}
		}
	}
	return accounts, copyString(r.updated)
}

func (r *CommandReader) ActiveAccountNumber() int {
	accounts, _ := r.Accounts()
	active := 0
	for _, account := range accounts {
		if account.Active {
			if active != 0 {
				return 0
			}
			active = account.Number
		}
	}
	return active
}

func (r *CommandReader) run(parent context.Context, now time.Time) ([]wire.AccountUsage, *string, error) {
	executable, err := r.resolve()
	if err != nil {
		return nil, nil, err
	}
	ctx, cancel := context.WithTimeout(parent, r.timeout)
	defer cancel()
	command := exec.CommandContext(ctx, executable, "list", "--json")
	var stdout boundedBuffer
	stdout.limit = r.maxOutput
	command.Stdout = &stdout
	command.Stderr = io.Discard
	command.Stdin = nil
	command.WaitDelay = time.Second
	if err := command.Run(); err != nil {
		if ctx.Err() != nil {
			return nil, nil, ctx.Err()
		}
		if stdout.exceeded {
			return nil, nil, errors.New("claude-swap output exceeds limit")
		}
		return nil, nil, errors.New("claude-swap command failed")
	}
	if stdout.exceeded {
		return nil, nil, errors.New("claude-swap output exceeds limit")
	}
	return parseCommandAccounts(bytes.TrimSpace(stdout.Bytes()), now)
}

func (r *CommandReader) resolve() (string, error) {
	if path, err := r.lookup("cswap"); err == nil && filepath.IsAbs(path) {
		return path, nil
	}
	candidate := filepath.Join(r.home, ".local", "bin", "cswap")
	path, err := r.lookup(candidate)
	if err != nil || !filepath.IsAbs(path) {
		return "", errors.New("claude-swap executable unavailable")
	}
	return path, nil
}

type boundedBuffer struct {
	bytes.Buffer
	limit    int
	exceeded bool
}

func (b *boundedBuffer) Write(p []byte) (int, error) {
	remaining := b.limit - b.Len()
	if remaining <= 0 {
		b.exceeded = true
		return len(p), nil
	}
	if len(p) > remaining {
		_, _ = b.Buffer.Write(p[:remaining])
		b.exceeded = true
		return len(p), nil
	}
	return b.Buffer.Write(p)
}

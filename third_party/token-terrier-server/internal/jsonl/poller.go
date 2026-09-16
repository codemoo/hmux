package jsonl

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/safefile"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const (
	maxJSONLLineBytes = 8 << 20
	maxTailReadBytes  = maxJSONLLineBytes + (64 << 10)
	maxJSONLFileBytes = 16 << 30
	// Startup reconstruction is bounded across all roots. Only files modified
	// today are considered, newest first, and parsed events are filtered to the
	// current local day before they reach the burn tracker.
	maxBootstrapBackfillBytes = 64 << 20
)

// Poller watches local JSONL session files and emits TokenEvents
// to the supplied callback. Mirrors Sources/TokenUsageCore/JSONL/JSONLPoller.swift
// in spirit (per-file offset tracking).
//
// Strategy: every PollInterval, list .jsonl files in the claude/codex roots.
// For files whose size grew since last poll, read the new tail and parse each line.
// Per-file offsets persist in memory; daemon restart re-scans from the
// current EOF (no historical replay — we don't want a startup spike of
// stale events distorting the burn rate).
type Poller struct {
	ClaudeRoot                string // path, e.g. ~/.claude/projects
	CodexRoot                 string // path, e.g. ~/.codex/sessions
	ClaudeSwapSessionsRoot    string // path, e.g. ~/.claude-swap-backup/sessions
	DisableClaudeSwapSessions bool
	PollInterval              time.Duration
	ReconcileInterval         time.Duration

	logger *slog.Logger
	emit   func(TokenEvent)

	mu            sync.Mutex
	offsets       map[string]int64 // path → bytes already consumed
	discarding    map[string]bool  // path → currently skipping an oversized line
	status        map[wire.Provider]Status
	lastReconcile map[string]time.Time
	hotDirs       map[string]map[string]struct{}
}

// Status is a privacy-safe, provider-specific summary for authenticated
// diagnostics. Paths and filenames are intentionally omitted.
type Status struct {
	Observed      bool
	State         string
	LastScanAt    *string
	LastSuccessAt *string
	LastErrorAt   *string
	LastErrorKind string
}

// NewPoller builds a Poller. Roots default to the standard Claude Code/codex
// locations under the current user's home dir.
func NewPoller(emit func(TokenEvent), logger *slog.Logger) *Poller {
	if logger == nil {
		logger = slog.Default()
	}
	home, _ := os.UserHomeDir()
	claudeRoot := os.Getenv("TOKEN_USAGE_CLAUDE_PROJECTS")
	if claudeRoot == "" {
		claudeRoot = filepath.Join(home, ".claude", "projects")
	}
	codexRoot := os.Getenv("TOKEN_USAGE_CODEX_SESSIONS")
	if codexRoot == "" {
		codexRoot = filepath.Join(home, ".codex", "sessions")
	}
	swapSessionsRoot := strings.TrimSpace(os.Getenv("TOKEN_USAGE_CLAUDE_SWAP_SESSIONS_ROOT"))
	if swapSessionsRoot == "" {
		swapSessionsRoot = filepath.Join(home, ".claude-swap-backup", "sessions")
	}
	return &Poller{
		ClaudeRoot:                claudeRoot,
		CodexRoot:                 codexRoot,
		ClaudeSwapSessionsRoot:    swapSessionsRoot,
		DisableClaudeSwapSessions: os.Getenv("TOKEN_USAGE_DISABLE_CLAUDE_SWAP") == "1" || os.Getenv("TOKEN_USAGE_DISABLE_CLAUDE_SWAP_SESSIONS") == "1",
		PollInterval:              5 * time.Second,
		ReconcileInterval:         5 * time.Minute,
		logger:                    logger,
		emit:                      emit,
		offsets:                   map[string]int64{},
		discarding:                map[string]bool{},
		status: map[wire.Provider]Status{
			wire.ProviderClaude: {State: "unobserved"},
			wire.ProviderCodex:  {State: "unobserved"},
		},
		lastReconcile: map[string]time.Time{},
		hotDirs:       map[string]map[string]struct{}{},
	}
}

// SetEmitter wires the downstream callback before Run starts.
func (p *Poller) SetEmitter(emit func(TokenEvent)) {
	p.mu.Lock()
	p.emit = emit
	p.mu.Unlock()
}

// Run blocks until ctx cancels, polling every PollInterval.
func (p *Poller) Run(ctx context.Context) {
	// First pass — establish offsets at current EOF for every existing
	// file so we don't replay history. Subsequent ticks see only new lines.
	p.bootstrapOffsets(ctx)
	p.tick(ctx)

	t := time.NewTicker(p.PollInterval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			p.tick(ctx)
		}
	}
}

func (p *Poller) tick(ctx context.Context) {
	type outcome struct {
		succeeded bool
		err       error
	}
	outcomes := map[wire.Provider]outcome{}
	for _, root := range p.pollRoots() {
		current := outcomes[root.provider]
		if err := p.tickRoot(ctx, root); err != nil {
			if current.err == nil {
				current.err = err
			}
		} else {
			current.succeeded = true
		}
		outcomes[root.provider] = current
	}
	for _, provider := range []wire.Provider{wire.ProviderClaude, wire.ProviderCodex} {
		current := outcomes[provider]
		p.recordScan(provider, current.succeeded, current.err, time.Now())
	}
}

// Status returns the latest scan result without filesystem I/O.
func (p *Poller) Status(provider wire.Provider) Status {
	p.mu.Lock()
	defer p.mu.Unlock()
	status, ok := p.status[provider]
	if !ok {
		return Status{State: "disabled"}
	}
	return status
}

func (p *Poller) recordScan(provider wire.Provider, succeeded bool, err error, now time.Time) {
	nowText := wire.FormatTime(now)
	p.mu.Lock()
	defer p.mu.Unlock()
	status := p.status[provider]
	status.Observed = true
	status.LastScanAt = &nowText
	if succeeded {
		status.State = "ok"
		status.LastSuccessAt = &nowText
		status.LastErrorKind = ""
	} else {
		status.State = "error"
		status.LastErrorAt = &nowText
		status.LastErrorKind = classifyScanError(err)
		if status.LastErrorKind == "missing" {
			status.State = "missing"
		} else if status.LastErrorKind == "permission" {
			status.State = "permission"
		}
	}
	p.status[provider] = status
}

func classifyScanError(err error) string {
	switch {
	case err == nil:
		return "no_roots"
	case errors.Is(err, os.ErrNotExist):
		return "missing"
	case errors.Is(err, os.ErrPermission):
		return "permission"
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded):
		return "canceled"
	default:
		return "scan_failed"
	}
}

func (p *Poller) bootstrapOffsets(ctx context.Context) {
	type candidate struct {
		root    pollRoot
		path    string
		size    int64
		modTime time.Time
	}
	var candidates []candidate
	dayStart := time.Now().In(time.Local)
	dayStart = time.Date(dayStart.Year(), dayStart.Month(), dayStart.Day(), 0, 0, 0, 0, dayStart.Location())
	for _, root := range p.pollRoots() {
		listing, err := p.listJSONL(ctx, root.path)
		if err != nil {
			p.logger.Warn("jsonl list failed during bootstrap", "root", root.path, "err", err)
			continue
		}
		p.rebuildHotDirs(root.path, listing, time.Now())
		p.mu.Lock()
		p.lastReconcile[root.path] = time.Now()
		p.mu.Unlock()
		p.mu.Lock()
		for path, size := range listing {
			p.offsets[path] = size
		}
		p.mu.Unlock()
		for path, size := range listing {
			info, statErr := os.Stat(path)
			if statErr == nil && size > 0 && !info.ModTime().Before(dayStart) {
				candidates = append(candidates, candidate{root: root, path: path, size: size, modTime: info.ModTime()})
			}
		}
	}
	sort.SliceStable(candidates, func(i, j int) bool { return candidates[i].modTime.After(candidates[j].modTime) })
	remaining := int64(maxBootstrapBackfillBytes)
	var backfilled int64
	for _, item := range candidates {
		if remaining <= 0 {
			break
		}
		readSize := item.size
		if readSize > int64(maxTailReadBytes) {
			readSize = int64(maxTailReadBytes)
		}
		if readSize > remaining {
			readSize = remaining
		}
		offset := item.size - readSize
		data, err := p.tailFrom(ctx, item.path, offset)
		if err != nil {
			p.logger.Debug("jsonl bootstrap backfill failed", "path", item.path, "err", err)
			continue
		}
		if offset > 0 {
			// The bounded tail normally starts in the middle of a line. Drop
			// that fragment so it can never be mistaken for an independent event.
			if nl := indexNL(data); nl >= 0 {
				data = data[nl+1:]
			} else {
				data = nil
			}
		}
		p.parseAndEmitSince(item.root, item.path, data, dayStart)
		used := int64(len(data))
		remaining -= readSize
		backfilled += used
	}
	p.logger.Info("jsonl poller bootstrap complete",
		"tracked_files", p.fileCount(),
		"backfilled_bytes", backfilled,
		"backfill_limit_bytes", maxBootstrapBackfillBytes)
}

func (p *Poller) fileCount() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return len(p.offsets)
}

type pollRoot struct {
	provider            wire.Provider
	path                string
	claudeAccountNumber int
}

func (p *Poller) pollRoots() []pollRoot {
	roots := []pollRoot{
		{provider: wire.ProviderClaude, path: p.ClaudeRoot},
	}
	if !p.DisableClaudeSwapSessions {
		roots = append(roots, discoverClaudeSwapProjectRoots(p.ClaudeSwapSessionsRoot)...)
	}
	roots = append(roots, pollRoot{provider: wire.ProviderCodex, path: p.CodexRoot})
	return dedupePollRoots(roots)
}

func dedupePollRoots(roots []pollRoot) []pollRoot {
	seen := map[string]struct{}{}
	out := make([]pollRoot, 0, len(roots))
	for _, root := range roots {
		clean := filepath.Clean(strings.TrimSpace(root.path))
		if clean == "" || clean == "." {
			continue
		}
		key := string(root.provider) + "\x00" + clean
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		root.path = clean
		out = append(out, root)
	}
	return out
}

func discoverClaudeSwapProjectRoots(sessionsRoot string) []pollRoot {
	sessionsRoot = filepath.Clean(strings.TrimSpace(sessionsRoot))
	if sessionsRoot == "" || sessionsRoot == "." {
		return nil
	}
	entries, err := os.ReadDir(sessionsRoot)
	if err != nil {
		return nil
	}
	roots := make([]pollRoot, 0, len(entries))
	for _, entry := range entries {
		if entry == nil || !entry.IsDir() {
			continue
		}
		accountNumber := parseClaudeSwapSessionAccountNumber(entry.Name())
		if accountNumber <= 0 {
			continue
		}
		projects := filepath.Join(sessionsRoot, entry.Name(), "projects")
		if info, err := os.Stat(projects); err == nil && info.IsDir() {
			roots = append(roots, pollRoot{
				provider:            wire.ProviderClaude,
				path:                projects,
				claudeAccountNumber: accountNumber,
			})
		}
	}
	sort.SliceStable(roots, func(i, j int) bool {
		return roots[i].claudeAccountNumber < roots[j].claudeAccountNumber
	})
	return roots
}

func parseClaudeSwapSessionAccountNumber(name string) int {
	if name == "" {
		return 0
	}
	i := 0
	for i < len(name) && name[i] >= '0' && name[i] <= '9' {
		i++
	}
	if i == 0 || i >= len(name) || name[i] != '-' {
		return 0
	}
	n, err := strconv.Atoi(name[:i])
	if err != nil || n <= 0 {
		return 0
	}
	return n
}

func (p *Poller) tickRoot(ctx context.Context, root pollRoot) error {
	fullReconcile := p.shouldReconcile(root.path, time.Now())
	var listing map[string]int64
	var err error
	if fullReconcile {
		listing, err = p.listJSONL(ctx, root.path)
	} else {
		listing, err = p.listHotJSONL(ctx, root.path)
	}
	if err != nil {
		p.logger.Debug("jsonl list failed", "provider", root.provider, "err", err)
		return err
	}
	// Prune offsets for files that disappeared upstream — codex CLI
	// rotates rollouts daily and Claude Code sessions get cleaned up by
	// the user's retention script. Without this the offset map grows
	// unbounded across months of uptime.
	if fullReconcile {
		p.pruneStaleOffsets(root.path, listing)
		p.rebuildHotDirs(root.path, listing, time.Now())
		p.mu.Lock()
		p.lastReconcile[root.path] = time.Now()
		p.mu.Unlock()
	}

	var firstErr error
	for path, currentSize := range listing {
		p.mu.Lock()
		prev, known := p.offsets[path]
		p.mu.Unlock()
		if !known {
			// Run() already baselined files that existed at startup. A file
			// first observed here is therefore a new live session; consume
			// complete lines written before this poll instead of dropping them.
			prev = 0
			p.mu.Lock()
			p.offsets[path] = 0
			p.mu.Unlock()
		}
		if currentSize < prev {
			// File shrank — likely a truncate/rotation. Reset to the
			// current EOF so we don't tail garbage offsets.
			p.mu.Lock()
			p.offsets[path] = currentSize
			p.mu.Unlock()
			continue
		}
		if currentSize == prev {
			continue
		}
		newBytes, err := p.tailFrom(ctx, path, prev)
		if err != nil {
			p.logger.Debug("jsonl tail failed", "path", path, "err", err)
			if firstErr == nil {
				firstErr = err
			}
			continue
		}
		consumed := p.parseAndEmit(root, path, newBytes)
		p.mu.Lock()
		p.offsets[path] = prev + int64(consumed)
		p.mu.Unlock()
	}
	return firstErr
}

func (p *Poller) shouldReconcile(root string, now time.Time) bool {
	interval := p.ReconcileInterval
	if interval <= 0 {
		interval = 5 * time.Minute
	}
	p.mu.Lock()
	last := p.lastReconcile[root]
	p.mu.Unlock()
	return last.IsZero() || now.Sub(last) >= interval
}

// listHotJSONL checks only directories that contained recently modified JSONL
// at the last full reconcile. Each directory read is non-recursive.
func (p *Poller) listHotJSONL(ctx context.Context, root string) (map[string]int64, error) {
	p.mu.Lock()
	dirs := make([]string, 0, len(p.hotDirs[root]))
	for dir := range p.hotDirs[root] {
		dirs = append(dirs, dir)
	}
	p.mu.Unlock()
	result := map[string]int64{}
	var firstErr error
	for _, dir := range dirs {
		select {
		case <-ctx.Done():
			return result, ctx.Err()
		default:
		}
		entries, err := os.ReadDir(dir)
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				continue
			}
			if firstErr == nil {
				firstErr = err
			}
			continue
		}
		for _, entry := range entries {
			if entry == nil || entry.IsDir() || entry.Type()&os.ModeSymlink != 0 ||
				filepath.Ext(entry.Name()) != ".jsonl" {
				continue
			}
			info, err := entry.Info()
			if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o022 != 0 ||
				info.Size() < 0 || info.Size() > maxJSONLFileBytes {
				continue
			}
			result[filepath.Join(dir, entry.Name())] = info.Size()
		}
	}
	return result, firstErr
}

func (p *Poller) rebuildHotDirs(root string, listing map[string]int64, now time.Time) {
	dirs := map[string]struct{}{root: {}}
	cutoff := now.Add(-24 * time.Hour)
	for path := range listing {
		info, err := os.Lstat(path)
		if err == nil && info.Mode().IsRegular() && !info.ModTime().Before(cutoff) {
			dirs[filepath.Dir(path)] = struct{}{}
		}
	}
	p.mu.Lock()
	p.hotDirs[root] = dirs
	p.mu.Unlock()
}

// pruneStaleOffsets removes offset entries for paths that are no longer in
// `listing`, but only for paths under `root` (so a Claude tick doesn't drop
// Codex offsets, and vice versa).
func (p *Poller) pruneStaleOffsets(root string, listing map[string]int64) {
	rootPrefix := root
	if !strings.HasSuffix(rootPrefix, "/") {
		rootPrefix += "/"
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	for path := range p.offsets {
		if !strings.HasPrefix(path, rootPrefix) {
			continue // not in this provider's root
		}
		if _, stillThere := listing[path]; !stillThere {
			delete(p.offsets, path)
			delete(p.discarding, path)
		}
	}
}

// listJSONL returns map[absPath] = size.
func (p *Poller) listJSONL(ctx context.Context, root string) (map[string]int64, error) {
	result := map[string]int64{}
	info, err := os.Stat(root)
	if err != nil {
		return result, err
	}
	if !info.IsDir() {
		return result, fmt.Errorf("jsonl root is not a directory")
	}
	err = filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			if errors.Is(walkErr, os.ErrNotExist) || errors.Is(walkErr, os.ErrPermission) {
				return nil
			}
			return walkErr
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
		if entry == nil || entry.IsDir() || entry.Type()&os.ModeSymlink != 0 || filepath.Ext(path) != ".jsonl" {
			return nil
		}
		info, err := entry.Info()
		if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o022 != 0 ||
			info.Size() < 0 || info.Size() > maxJSONLFileBytes {
			return nil
		}
		result[path] = info.Size()
		return nil
	})
	return result, err
}

// tailFrom returns a bounded chunk from offset. A large append is drained
// over multiple polls so one pathological session file cannot force an
// unbounded allocation.
func (p *Poller) tailFrom(ctx context.Context, path string, offset int64) ([]byte, error) {
	file, info, err := safefile.Open(path, maxJSONLFileBytes)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	if offset < 0 || offset > info.Size() {
		return nil, errors.New("jsonl offset is outside the file")
	}
	if _, err := file.Seek(offset, io.SeekStart); err != nil {
		return nil, err
	}
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	default:
	}
	data, readErr := io.ReadAll(io.LimitReader(file, maxTailReadBytes))
	if readErr != nil {
		return nil, readErr
	}
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	default:
		return data, nil
	}
}

// parseAndEmit splits on newlines, parses each, emits TokenEvents. Returns
// the number of bytes "consumed" — strictly less than len(buf) when the
// last line is partial (no trailing newline yet). The trailing fragment is
// re-read on the next tick when the rest arrives.
func (p *Poller) parseAndEmit(root pollRoot, path string, buf []byte) int {
	return p.parseAndEmitSince(root, path, buf, time.Time{})
}

func (p *Poller) parseAndEmitSince(root pollRoot, path string, buf []byte, since time.Time) int {
	consumed := 0
	p.mu.Lock()
	discarding := p.discarding[path]
	p.mu.Unlock()
	for {
		remaining := buf[consumed:]
		nl := indexNL(remaining)
		if discarding {
			if nl < 0 {
				consumed = len(buf)
				break
			}
			consumed += nl + 1
			discarding = false
			continue
		}
		if nl < 0 {
			if len(remaining) > maxJSONLLineBytes {
				// Consume this chunk and remain in discard mode until a future
				// chunk reaches the newline that terminates the oversized line.
				consumed = len(buf)
				discarding = true
			}
			break
		}
		end := consumed + nl
		line := buf[consumed:end]
		consumed = end + 1 // skip the '\n'
		if len(line) > maxJSONLLineBytes {
			continue
		}
		if ev := ParseLine(root.provider, line, path); ev != nil && (since.IsZero() || !ev.Timestamp.Before(since)) {
			ev.AccountNumber = root.claudeAccountNumber
			p.emit(*ev)
		}
	}
	p.mu.Lock()
	if discarding {
		p.discarding[path] = true
	} else {
		delete(p.discarding, path)
	}
	p.mu.Unlock()
	return consumed
}

func indexNL(b []byte) int {
	for i, c := range b {
		if c == '\n' {
			return i
		}
	}
	return -1
}

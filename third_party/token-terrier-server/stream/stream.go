// Package stream exposes Token Terrier snapshots as a bounded stdio stream.
//
// It is intentionally authentication-free at the protocol layer: callers
// must provide their own authenticated transport (HMux uses its existing
// local-user boundary or SSH connection). Provider credentials are read and
// refreshed only on the machine running this package and are never serialized.
package stream

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"math"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/burn"
	"github.com/codemoo/token-terrier/server-go/internal/claudeswap"
	"github.com/codemoo/token-terrier/server-go/internal/codexaccounts"
	"github.com/codemoo/token-terrier/server-go/internal/codexlb"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/state"
	"github.com/codemoo/token-terrier/server-go/internal/usage"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const (
	ProtocolVersion   = 1
	MaximumFrameBytes = 1 << 20
	HeartbeatInterval = 10 * time.Second
	SourceLease       = 35 * time.Second
	snapshotInterval  = time.Second
	refreshInterval   = 60 * time.Second
	refreshTimeout    = 25 * time.Second
	maximumKeyBytes   = 64 << 10
)

// Frame is the only shape sent between a Home agent and an HMux client.
// Snapshot contains quota/activity data, never OAuth/API credentials.
type Frame struct {
	ProtocolVersion int             `json:"protocol_version"`
	Sequence        uint64          `json:"sequence"`
	Type            string          `json:"type"`
	Code            string          `json:"code,omitempty"`
	Provider        string          `json:"provider,omitempty"`
	Snapshot        json.RawMessage `json:"snapshot,omitempty"`
}

// transportSnapshot is the complete allowlist for bytes permitted to leave
// Home. It deliberately excludes producer identity, provider raw extras,
// credentials, credits and quota fields the HMux UI does not consume.
type transportSnapshot struct {
	Schema            int                `json:"schema"`
	Sequence          int                `json:"seq"`
	GeneratedAtUTC    string             `json:"generated_at_utc"`
	Provider          wire.Provider      `json:"provider"`
	BurnRatePerMinute float64            `json:"burn_rate_per_min"`
	BurnState         string             `json:"burn_state"`
	TodayTotalTokens  int                `json:"today_total_tokens"`
	TodaySessions     int                `json:"today_sessions"`
	Rolling5h         wire.RollingWindow `json:"rolling_5h"`
	Weekly            wire.RollingWindow `json:"weekly"`
	Rolling5hObserved bool               `json:"rolling_5h_observed"`
	WeeklyObserved    bool               `json:"weekly_observed"`
	Status            transportStatus    `json:"status"`
	Accounts          []transportAccount `json:"accounts,omitempty"`
	AccountsUpdated   *string            `json:"accounts_updated_at,omitempty"`
}

type transportStatus struct {
	State           wire.ProviderState      `json:"state"`
	DataSource      wire.SnapshotDataSource `json:"data_source"`
	QuotaSource     wire.QuotaSource        `json:"quota_source"`
	Stale           bool                    `json:"stale"`
	QuotaObservedAt *string                 `json:"quota_observed_at,omitempty"`
	RetryAt         *string                 `json:"retry_at,omitempty"`
}

type transportAccount struct {
	Number        int                 `json:"number"`
	Email         string              `json:"email"`
	DisplayName   string              `json:"display_name,omitempty"`
	Active        bool                `json:"active"`
	Status        string              `json:"status"`
	FiveHour      *wire.AccountWindow `json:"five_hour"`
	SevenDay      *wire.AccountWindow `json:"seven_day"`
	TokensPerHour *float64            `json:"tokens_per_hour,omitempty"`
	TotalTokens   *int64              `json:"total_tokens,omitempty"`
	LastRefreshAt *string             `json:"last_refresh_at,omitempty"`
}

func newTransportSnapshot(snapshot wire.UsageSnapshot) transportSnapshot {
	accounts := make([]transportAccount, 0, len(snapshot.Accounts))
	for _, account := range snapshot.Accounts {
		accounts = append(accounts, transportAccount{
			Number:        account.Number,
			Email:         accountEmail(snapshot.Provider, account.Email),
			DisplayName:   accountDisplayName(account.DisplayName),
			Active:        account.Active,
			Status:        account.Status,
			FiveHour:      account.FiveHour,
			SevenDay:      account.SevenDay,
			TokensPerHour: account.TokensPerHour,
			TotalTokens:   account.TotalTokens,
			LastRefreshAt: account.LastRefreshAt,
		})
	}
	if len(accounts) == 0 {
		accounts = nil
	}
	return transportSnapshot{
		Schema:            snapshot.Schema,
		Sequence:          snapshot.Seq,
		GeneratedAtUTC:    snapshot.GeneratedAtUTC,
		Provider:          snapshot.Provider,
		BurnRatePerMinute: snapshot.BurnRatePerMinute,
		BurnState:         snapshot.BurnState,
		TodayTotalTokens:  snapshot.TodayTotalTokens,
		TodaySessions:     snapshot.TodaySessions,
		Rolling5h:         snapshot.Rolling5h,
		Weekly:            snapshot.Weekly,
		Rolling5hObserved: snapshot.Rolling5hObserved,
		WeeklyObserved:    snapshot.WeeklyObserved,
		Status: transportStatus{
			State:           snapshot.Status.State,
			DataSource:      snapshot.Status.DataSource,
			QuotaSource:     snapshot.Status.QuotaSource,
			Stale:           snapshot.Status.Stale,
			QuotaObservedAt: snapshot.Status.QuotaObservedAt,
			RetryAt:         snapshot.Status.RetryAt,
		},
		Accounts:        accounts,
		AccountsUpdated: snapshot.AccountsUpdated,
	}
}

func decodeTransportSnapshot(raw []byte) (transportSnapshot, error) {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	var snapshot transportSnapshot
	if err := decoder.Decode(&snapshot); err != nil {
		return transportSnapshot{}, err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return transportSnapshot{}, errors.New("usage snapshot contains trailing data")
	}
	if err := snapshot.validate(); err != nil {
		return transportSnapshot{}, err
	}
	return snapshot, nil
}

func (s transportSnapshot) validate() error {
	if s.Schema != 1 || s.Sequence < 0 ||
		(s.Provider != wire.ProviderClaude && s.Provider != wire.ProviderCodex) ||
		len(s.GeneratedAtUTC) == 0 || len(s.GeneratedAtUTC) > 128 ||
		len(s.BurnState) > 32 || math.IsNaN(s.BurnRatePerMinute) || math.IsInf(s.BurnRatePerMinute, 0) ||
		s.BurnRatePerMinute < 0 || s.TodayTotalTokens < 0 || s.TodaySessions < 0 ||
		!validTransportWindow(s.Rolling5h) || !validTransportWindow(s.Weekly) ||
		len(s.Status.State) == 0 || len(s.Status.State) > 64 ||
		len(s.Status.DataSource) > 128 || len(s.Status.QuotaSource) > 128 ||
		stringLength(s.Status.QuotaObservedAt) > 128 ||
		!validTransportRetryAt(s.Status.RetryAt, s.GeneratedAtUTC, s.Status.State, s.Status.Stale) ||
		stringLength(s.AccountsUpdated) > 128 ||
		len(s.Accounts) > 128 {
		return errors.New("invalid usage snapshot payload")
	}
	accountNumbers := make(map[int]bool, len(s.Accounts))
	for _, account := range s.Accounts {
		if account.Number <= 0 || accountNumbers[account.Number] || account.Email != accountEmail(s.Provider, account.Email) ||
			account.DisplayName != accountDisplayName(account.DisplayName) || len(account.Status) > 64 ||
			!validTransportAccountWindow(account.FiveHour) || !validTransportAccountWindow(account.SevenDay) ||
			(account.TokensPerHour != nil && (math.IsNaN(*account.TokensPerHour) || math.IsInf(*account.TokensPerHour, 0) || *account.TokensPerHour < 0)) ||
			(account.TotalTokens != nil && *account.TotalTokens < 0) || stringLength(account.LastRefreshAt) > 128 {
			return errors.New("invalid usage snapshot account")
		}
		accountNumbers[account.Number] = true
	}
	return nil
}

func validTransportRetryAt(value *string, generatedAt string, state wire.ProviderState, stale bool) bool {
	if value == nil {
		return true
	}
	if state != wire.StateRateLimited && (state != wire.StateOK || !stale) {
		return false
	}
	if len(*value) == 0 || len(*value) > 128 {
		return false
	}
	retryAt, err := time.Parse(time.RFC3339Nano, *value)
	if err != nil {
		return false
	}
	generated, err := time.Parse(time.RFC3339Nano, generatedAt)
	if err != nil {
		return false
	}
	delay := retryAt.Sub(generated)
	return delay > 0 && delay <= 24*time.Hour
}

// The owner explicitly requested cswap email labels. Codex continues to use
// aliases only; credentials and account IDs are excluded.
func accountEmail(provider wire.Provider, value string) string {
	if provider != wire.ProviderClaude {
		return ""
	}
	return accountDisplayName(value)
}

func accountDisplayName(value string) string {
	value = strings.TrimSpace(value)
	if !utf8.ValidString(value) || len(value) > 256 {
		return ""
	}
	for _, r := range value {
		if unicode.IsControl(r) || (r >= 0x202a && r <= 0x202e) || (r >= 0x2066 && r <= 0x2069) {
			return ""
		}
	}
	return value
}

func validTransportWindow(window wire.RollingWindow) bool {
	return !math.IsNaN(window.UsedPct) && !math.IsInf(window.UsedPct, 0) &&
		window.UsedPct >= 0 && window.UsedPct <= 1 && window.RemainingSeconds >= 0 &&
		stringLength(window.ResetsAt) <= 128
}

func validTransportAccountWindow(window *wire.AccountWindow) bool {
	return window == nil || (!math.IsNaN(window.UsedPct) && !math.IsInf(window.UsedPct, 0) &&
		window.UsedPct >= 0 && window.UsedPct <= 1 && stringLength(window.ResetsAt) <= 128)
}

func stringLength(value *string) int {
	if value == nil {
		return 0
	}
	return len(*value)
}

func (f Frame) Validate() error {
	if f.ProtocolVersion != ProtocolVersion || f.Sequence < 1 {
		return errors.New("invalid usage stream protocol or sequence")
	}
	switch f.Type {
	case "heartbeat":
		if f.Code != "" || f.Provider != "" || len(f.Snapshot) != 0 {
			return errors.New("heartbeat contains unsolicited data")
		}
		return nil
	case "status":
		if f.Code != "home_agent_update_required" || f.Provider != "" || len(f.Snapshot) != 0 {
			return errors.New("invalid usage stream status")
		}
		return nil
	case "snapshot":
		if f.Code != "" {
			return errors.New("usage snapshot contains unsolicited status")
		}
		if f.Provider != string(wire.ProviderClaude) && f.Provider != string(wire.ProviderCodex) {
			return errors.New("invalid usage snapshot provider")
		}
		if len(f.Snapshot) == 0 || len(f.Snapshot) > MaximumFrameBytes || !json.Valid(f.Snapshot) {
			return errors.New("invalid usage snapshot payload")
		}
		snapshot, err := decodeTransportSnapshot(f.Snapshot)
		if err != nil || string(snapshot.Provider) != f.Provider {
			return errors.New("usage snapshot identity mismatch")
		}
		return nil
	default:
		return errors.New("invalid usage stream frame type")
	}
}

type Encoder struct {
	encoder *json.Encoder
}

func NewEncoder(writer io.Writer) (*Encoder, error) {
	if writer == nil {
		return nil, errors.New("usage stream writer is required")
	}
	return &Encoder{encoder: json.NewEncoder(writer)}, nil
}

func (e *Encoder) Encode(frame Frame) error {
	if e == nil || e.encoder == nil {
		return errors.New("usage stream encoder is unavailable")
	}
	if err := frame.Validate(); err != nil {
		return err
	}
	encoded, err := json.Marshal(frame)
	if err != nil {
		return err
	}
	if len(encoded)+1 > MaximumFrameBytes {
		return errors.New("usage stream frame exceeds size limit")
	}
	return e.encoder.Encode(frame)
}

type Decoder struct {
	reader *bufio.Reader
}

func NewDecoder(reader io.Reader) (*Decoder, error) {
	if reader == nil {
		return nil, errors.New("usage stream reader is required")
	}
	return &Decoder{reader: bufio.NewReaderSize(reader, MaximumFrameBytes+1)}, nil
}

func (d *Decoder) Decode() (Frame, error) {
	if d == nil || d.reader == nil {
		return Frame{}, errors.New("usage stream decoder is unavailable")
	}
	line, err := d.reader.ReadSlice('\n')
	if errors.Is(err, bufio.ErrBufferFull) || len(line) > MaximumFrameBytes {
		return Frame{}, errors.New("usage stream frame exceeds size limit")
	}
	if err != nil {
		if errors.Is(err, io.EOF) && len(line) > 0 {
			return Frame{}, io.ErrUnexpectedEOF
		}
		return Frame{}, err
	}
	line = bytes.TrimSuffix(line, []byte{'\n'})
	line = bytes.TrimSuffix(line, []byte{'\r'})
	if len(line) == 0 {
		return Frame{}, errors.New("empty usage stream frame")
	}
	decoder := json.NewDecoder(bytes.NewReader(line))
	decoder.DisallowUnknownFields()
	var frame Frame
	if err := decoder.Decode(&frame); err != nil {
		return Frame{}, fmt.Errorf("decode usage stream frame: %w", err)
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return Frame{}, errors.New("usage stream frame contains trailing data")
	}
	if err := frame.Validate(); err != nil {
		return Frame{}, err
	}
	return frame, nil
}

// Run collects quota and local activity from the current Home user and writes
// a finite, validated NDJSON protocol until ctx is canceled or writer fails.
func Run(ctx context.Context, writer io.Writer) error {
	if ctx == nil {
		return errors.New("usage stream context is required")
	}
	encoder, err := NewEncoder(writer)
	if err != nil {
		return err
	}
	runtime, err := newRuntime(ctx)
	if err != nil {
		return err
	}
	defer runtime.stop()

	sequence := uint64(0)
	heartbeat := time.NewTicker(HeartbeatInterval)
	defer heartbeat.Stop()
	snapshotTicker := time.NewTicker(snapshotInterval)
	defer snapshotTicker.Stop()
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-snapshotTicker.C:
			for _, snapshot := range runtime.takeSnapshots() {
				raw, marshalErr := json.Marshal(newTransportSnapshot(snapshot))
				if marshalErr != nil {
					return marshalErr
				}
				sequence++
				if err := encoder.Encode(Frame{
					ProtocolVersion: ProtocolVersion,
					Sequence:        sequence,
					Type:            "snapshot",
					Provider:        string(snapshot.Provider),
					Snapshot:        raw,
				}); err != nil {
					return err
				}
			}
		case <-heartbeat.C:
			sequence++
			if err := encoder.Encode(Frame{
				ProtocolVersion: ProtocolVersion,
				Sequence:        sequence,
				Type:            "heartbeat",
			}); err != nil {
				return err
			}
		}
	}
}

type collectorRuntime struct {
	cancel context.CancelFunc
	wg     sync.WaitGroup

	snapshotMu sync.Mutex
	latest     map[wire.Provider]wire.UsageSnapshot
	dirty      map[wire.Provider]bool
}

// readOnlyCredentialSource makes the Home CLI credential store structurally
// read-only to HMux. Claude/Codex remain the only processes allowed to rotate
// their sessions; revision tracking makes an external login or account switch
// authoritative on the next collection pass.
type readOnlyCredentialSource struct {
	source *auth.LocalSource
}

func newReadOnlyCredentialSource(source *auth.LocalSource) *readOnlyCredentialSource {
	return &readOnlyCredentialSource{source: source}
}

func (s *readOnlyCredentialSource) Read(ctx context.Context, provider wire.Provider) ([]byte, error) {
	return s.source.Read(ctx, provider)
}

func (s *readOnlyCredentialSource) Revision(ctx context.Context, provider wire.Provider) (auth.SourceRevision, error) {
	return s.source.Revision(ctx, provider)
}

func (*readOnlyCredentialSource) Write(context.Context, wire.Provider, []byte) error {
	return errors.New("HMux usage credentials are read-only")
}

func newRuntime(parent context.Context) (*collectorRuntime, error) {
	home, err := os.UserHomeDir()
	if err != nil || !filepath.IsAbs(home) {
		return nil, errors.New("usage Home directory is unavailable")
	}
	ctx, cancel := context.WithCancel(parent)
	runtime := &collectorRuntime{
		cancel: cancel,
		latest: map[wire.Provider]wire.UsageSnapshot{},
		dirty:  map[wire.Provider]bool{},
	}
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	producer := wire.ProducerInfo{ID: "hmux-home", TimeZone: localTimeZone()}
	credentialSource := newReadOnlyCredentialSource(&auth.LocalSource{
		ClaudePath: filepath.Join(home, ".claude", ".credentials.json"),
		CodexPath:  filepath.Join(home, ".codex", "auth.json"),
	})
	credentialStore := auth.NewCredentialStore(credentialSource)
	usageClient := usage.NewClient(producer)
	now := time.Now()
	claudeState := state.New(
		wire.ProviderClaude, credentialStore, usageClient, nil,
		burn.New(time.Local, now), producer, logger,
	)
	codexState := state.New(
		wire.ProviderCodex, credentialStore, usageClient, nil,
		burn.New(time.Local, now), producer, logger,
	)
	codexState.SetLocalSnapshotter(codexlb.NewSnapshotterWithConfiguration(
		producer, logger, "", loadCodexLBKey(home),
	))

	claudeSwapEnabled := os.Getenv("TOKEN_USAGE_DISABLE_CLAUDE_SWAP") != "1"
	var claudeActivity *claudeswap.ActivityTracker
	var claudeAccounts interface {
		state.AccountsProvider
		ActiveAccountNumber() int
		SetActivityProvider(claudeswap.ActivityProvider)
	}
	if claudeSwapEnabled {
		path := strings.TrimSpace(os.Getenv("TOKEN_USAGE_CLAUDE_SWAP_ACCOUNTS"))
		if path == "" {
			path = filepath.Join(home, ".config", "token-usage", "claude-swap-accounts.json")
		}
		claudeActivity = claudeswap.NewActivityTracker(time.Local, now)
		if strings.TrimSpace(os.Getenv("TOKEN_USAGE_CLAUDE_SWAP_ACCOUNTS")) != "" {
			claudeAccounts = claudeswap.NewReader(path, logger)
		} else {
			claudeAccounts = claudeswap.NewNativeReader(home, path, logger)
		}
		claudeAccounts.SetActivityProvider(claudeActivity)
		claudeState.SetAccountsProvider(claudeAccounts)
	}
	if os.Getenv("TOKEN_USAGE_DISABLE_CODEX_ACCOUNTS") != "1" {
		path := strings.TrimSpace(os.Getenv("TOKEN_USAGE_CODEX_ACCOUNTS"))
		if path == "" {
			path = filepath.Join(home, ".config", "token-usage", "codex-lb-accounts.json")
		}
		codexState.SetAccountsProvider(codexaccounts.NewReader(path, logger))
	}

	publish := func(snapshot wire.UsageSnapshot) {
		// Never expose a machine hostname even if an upstream producer default
		// changes; the stream needs only quota/activity semantics.
		snapshot = sanitizeSnapshot(snapshot)
		runtime.publish(snapshot)
	}

	if os.Getenv("TOKEN_USAGE_DISABLE_JSONL") != "1" {
		poller := jsonl.NewPoller(nil, logger)
		poller.SetEmitter(func(event jsonl.TokenEvent) {
			event.Source = "jsonl"
			switch event.Provider {
			case wire.ProviderClaude:
				if claudeActivity != nil {
					if event.AccountNumber <= 0 && claudeAccounts != nil {
						event.AccountNumber = claudeAccounts.ActiveAccountNumber()
					}
					claudeActivity.Ingest(event, time.Now())
				}
				publish(claudeState.IngestEvent(event, time.Now()))
			case wire.ProviderCodex:
				publish(codexState.IngestEvent(event, time.Now()))
			}
		})
		runtime.wg.Add(1)
		go func() {
			defer runtime.wg.Done()
			poller.Run(ctx)
		}()
	}

	startRefreshWorker(ctx, &runtime.wg, claudeState, publish)
	startRefreshWorker(ctx, &runtime.wg, codexState, publish)
	return runtime, nil
}

func (r *collectorRuntime) publish(snapshot wire.UsageSnapshot) {
	if r == nil {
		return
	}
	r.snapshotMu.Lock()
	r.latest[snapshot.Provider] = snapshot
	r.dirty[snapshot.Provider] = true
	r.snapshotMu.Unlock()
}

func (r *collectorRuntime) takeSnapshots() []wire.UsageSnapshot {
	if r == nil {
		return nil
	}
	r.snapshotMu.Lock()
	defer r.snapshotMu.Unlock()
	result := make([]wire.UsageSnapshot, 0, 2)
	for _, provider := range []wire.Provider{wire.ProviderClaude, wire.ProviderCodex} {
		if !r.dirty[provider] {
			continue
		}
		result = append(result, r.latest[provider])
		delete(r.dirty, provider)
	}
	return result
}

func sanitizeSnapshot(snapshot wire.UsageSnapshot) wire.UsageSnapshot {
	snapshot.ProducerID = "hmux-home"
	snapshot.Extras.AccountEmail = nil
	for index := range snapshot.Accounts {
		snapshot.Accounts[index].Email = accountEmail(snapshot.Provider, snapshot.Accounts[index].Email)
	}
	return snapshot
}

func (r *collectorRuntime) stop() {
	if r == nil || r.cancel == nil {
		return
	}
	r.cancel()
	r.wg.Wait()
}

func startRefreshWorker(
	ctx context.Context,
	wg *sync.WaitGroup,
	usageState *state.State,
	publish func(wire.UsageSnapshot),
) {
	wg.Add(1)
	go func() {
		defer wg.Done()
		refresh := func() {
			refreshCtx, cancel := context.WithTimeout(ctx, refreshTimeout)
			update := usageState.Refresh(refreshCtx, time.Now())
			cancel()
			publish(update.Snapshot)
		}
		refresh()
		ticker := time.NewTicker(refreshInterval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				refresh()
			}
		}
	}()
}

func localTimeZone() string {
	zone := time.Local.String()
	if zone == "" || zone == "Local" {
		return "UTC"
	}
	return zone
}

func loadCodexLBKey(home string) string {
	for _, candidate := range []string{
		os.Getenv("TOKEN_USAGE_CODEX_LB_API_KEY"),
		os.Getenv("CODEX_LB_API_KEY"),
	} {
		if value := normalizeSecret(candidate); value != "" {
			return value
		}
	}
	data, err := readPrivateRegularFile(filepath.Join(home, ".codex", "lb-api-key"))
	if err != nil {
		return ""
	}
	return normalizeSecret(string(data))
}

func normalizeSecret(value string) string {
	value = strings.TrimSpace(value)
	if len(value) == 0 || len(value) > 4096 {
		return ""
	}
	for _, character := range value {
		if character < 0x21 || character > 0x7e {
			return ""
		}
	}
	return value
}

func readPrivateRegularFile(path string) ([]byte, error) {
	before, err := os.Lstat(path)
	if err != nil || !before.Mode().IsRegular() || before.Mode().Perm()&0o077 != 0 ||
		before.Size() < 1 || before.Size() > maximumKeyBytes {
		return nil, errors.New("private key file is unsafe")
	}
	beforeStat, ok := before.Sys().(*syscall.Stat_t)
	if !ok || int(beforeStat.Uid) != os.Getuid() || beforeStat.Nlink != 1 {
		return nil, errors.New("private key file ownership is unsafe")
	}
	descriptor, err := syscall.Open(path, syscall.O_RDONLY|syscall.O_CLOEXEC|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return nil, err
	}
	file := os.NewFile(uintptr(descriptor), path)
	if file == nil {
		_ = syscall.Close(descriptor)
		return nil, errors.New("open private key file")
	}
	defer file.Close()
	after, err := file.Stat()
	if err != nil {
		return nil, err
	}
	afterStat, ok := after.Sys().(*syscall.Stat_t)
	if !ok || afterStat.Dev != beforeStat.Dev || afterStat.Ino != beforeStat.Ino ||
		afterStat.Uid != beforeStat.Uid || afterStat.Nlink != beforeStat.Nlink ||
		after.Size() != before.Size() || !after.Mode().IsRegular() {
		return nil, errors.New("private key file changed while opening")
	}
	data, err := io.ReadAll(io.LimitReader(file, maximumKeyBytes+1))
	if err != nil || len(data) != int(after.Size()) || len(data) > maximumKeyBytes {
		return nil, errors.New("read private key file")
	}
	return data, nil
}

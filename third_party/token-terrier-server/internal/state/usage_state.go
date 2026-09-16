// Package state owns provider snapshots: cache TTLs, sticky last-good
// recovery for transient errors, rate-limit backoff, account-keyed
// invalidation, and the auth-expired transition signal.
//
// Mirrors Sources/TokenUsageCore/State/UsageState.swift behaviour.
package state

import (
	"context"
	"errors"
	"log/slog"
	"strings"
	"sync"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/burn"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/usage"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

// UsageUpdate is the result of a refresh attempt. Provider state transitions
// are represented only by Snapshot.Status.State; there is no parallel control
// frame that can disagree with the snapshot.
type UsageUpdate struct {
	Snapshot wire.UsageSnapshot
}

// Diagnostics is a privacy-safe summary of quota refresh state. It records
// categories and timestamps, never credential material or upstream response
// bodies, so API handlers can expose it to an authenticated caller.
type Diagnostics struct {
	Observed                  bool                `json:"observed"`
	State                     *wire.ProviderState `json:"state"`
	LastQuotaAttemptAt        *string             `json:"last_quota_attempt_at"`
	LastQuotaSuccessAt        *string             `json:"last_quota_success_at"`
	LastQuotaErrorAt          *string             `json:"last_quota_error_at"`
	LastQuotaErrorKind        string              `json:"last_quota_error_kind,omitempty"`
	ConsecutiveAuthRejections int                 `json:"consecutive_auth_rejections"`
}

// State holds the cached snapshot for one provider plus the bookkeeping that
// keeps the daemon from hammering upstream APIs and from blanking the UI on
// transient errors.
type State struct {
	mu sync.Mutex
	// refreshFlight coalesces concurrent snapshot requests for this provider.
	// The first caller performs credential and upstream I/O; followers observe
	// the same completed result instead of creating an upstream request storm.
	refreshFlight *refreshFlight

	provider    wire.Provider
	credentials *auth.CredentialStore
	usageClient *usage.Client
	localUsage  LocalSnapshotter
	accounts    AccountsProvider
	refresher   Refresher
	burn        *burn.Tracker
	producer    wire.ProducerInfo
	logger      *slog.Logger

	cacheTTL         time.Duration
	stickyTTL        time.Duration
	rateLimitBackoff time.Duration

	seq              int
	latestSnapshot   *wire.UsageSnapshot
	lastState        *wire.ProviderState
	lastFetchAt      time.Time
	lastOkSnapshot   *wire.UsageSnapshot
	lastOkAt         time.Time
	cacheAccountKey  string
	lastOkAccountKey string

	fetchSuspendedUntil      time.Time
	fetchSuspendedAccountKey string
	lastQuotaAttemptAt       time.Time
	lastQuotaSuccessAt       time.Time
	lastQuotaErrorAt         time.Time
	lastQuotaErrorKind       string

	// consecutiveAuthExpired counts back-to-back upstream auth rejections
	// that remained unresolved after disk reload and OAuth recovery. A
	// displayed authExpired state caused by missing credentials is explicitly
	// not one of these failures.
	consecutiveAuthExpired int
}

type refreshFlight struct {
	done   chan struct{}
	update UsageUpdate
}

// Refresher abstracts the OAuth refresher so Day 4's full implementation
// can be wired in without changing the state package surface. Day 3 uses
// a no-op that just returns the existing credential.
type Refresher interface {
	Refresh(ctx context.Context, c auth.OAuthCredential) (auth.OAuthCredential, error)
}

// LocalSnapshotter optionally supplies a provider snapshot from a local
// sidecar/store before the daemon falls back to the upstream usage API.
type LocalSnapshotter interface {
	Snapshot(ctx context.Context, seq int, now time.Time) (wire.UsageSnapshot, bool)
}

// AccountsProvider optionally supplies normalized per-account usage rows for
// the State's provider (for example claude-swap or codex-lb accounts).
type AccountsProvider interface {
	Accounts() ([]wire.AccountUsage, *string)
}

// New builds a State for one provider with daemon defaults.
func New(provider wire.Provider, credentials *auth.CredentialStore, usageClient *usage.Client, refresher Refresher, burnTracker *burn.Tracker, producer wire.ProducerInfo, logger *slog.Logger) *State {
	if logger == nil {
		logger = slog.Default()
	}
	if burnTracker == nil {
		burnTracker = burn.New(time.Local, time.Now())
	}
	return &State{
		provider:         provider,
		credentials:      credentials,
		usageClient:      usageClient,
		refresher:        refresher,
		burn:             burnTracker,
		producer:         producer,
		logger:           logger,
		cacheTTL:         60 * time.Second,
		stickyTTL:        600 * time.Second,
		rateLimitBackoff: 300 * time.Second,
	}
}

// SetLocalSnapshotter configures a local snapshot source for this provider.
func (s *State) SetLocalSnapshotter(snapshotter LocalSnapshotter) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.localUsage = snapshotter
}

// SetAccountsProvider configures the per-account usage source for this State.
func (s *State) SetAccountsProvider(p AccountsProvider) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.accounts = p
}

// decorateAccounts attaches accounts[] to a snapshot when an accounts
// provider is configured for this State's provider (Claude via claude-swap,
// Codex via codex-lb). Provider-agnostic: it trusts that s.accounts was
// wired to match this State's provider (see main.go's SetAccountsProvider
// call sites) — decorateAccounts itself does not gate on wire.Provider.
// No-op when no provider is set. MUST be called with s.mu UNLOCKED.
func (s *State) decorateAccounts(snap wire.UsageSnapshot) wire.UsageSnapshot {
	s.mu.Lock()
	ap := s.accounts
	s.mu.Unlock()
	if ap == nil {
		return snap
	}
	accts, updated := ap.Accounts()
	snap.Accounts = accts
	snap.AccountsUpdated = updated
	return snap
}

// Refresh runs the fetch/cache/sticky pipeline, then attaches accounts[].
func (s *State) Refresh(ctx context.Context, now time.Time) UsageUpdate {
	s.mu.Lock()
	if flight := s.refreshFlight; flight != nil {
		s.mu.Unlock()
		select {
		case <-flight.done:
			u := flight.update
			u.Snapshot = s.decorateAccounts(u.Snapshot)
			return u
		case <-ctx.Done():
			return UsageUpdate{Snapshot: s.Latest(now)}
		}
	}
	flight := &refreshFlight{done: make(chan struct{})}
	s.refreshFlight = flight
	s.mu.Unlock()

	u := s.refreshInner(ctx, now)
	s.mu.Lock()
	flight.update = u
	s.refreshFlight = nil
	close(flight.done)
	s.mu.Unlock()
	u.Snapshot = s.decorateAccounts(u.Snapshot)
	return u
}

// IngestEvent records a token event, then attaches accounts[].
func (s *State) IngestEvent(ev jsonl.TokenEvent, now time.Time) wire.UsageSnapshot {
	return s.decorateAccounts(s.ingestEventInner(ev, now))
}

// Latest returns the cached snapshot with live burn + accounts[].
func (s *State) Latest(now time.Time) wire.UsageSnapshot {
	return s.decorateAccounts(s.latestInner(now))
}

// ingestEventInner records a JSONL token event and returns the resulting
// snapshot (with bumped seq + fresh burn rate). The daemon's main routes
// this through the SSE hub so menubar clients see live burn rate updates.
func (s *State) ingestEventInner(ev jsonl.TokenEvent, now time.Time) wire.UsageSnapshot {
	burnSnap := s.burn.Ingest(ev, now)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.seq++
	base := s.latestSnapshot
	if base == nil {
		state := wire.StateNetworkError
		if s.lastState != nil {
			state = *s.lastState
		}
		degraded := wire.Degraded(s.provider, s.seq, s.producer, now, state)
		base = &degraded
	}
	merged := mergeBurn(*base, burnSnap, s.seq, now)
	merged.Status.RetryAt = retryAtPointer(s.fetchSuspendedUntil, now)
	s.latestSnapshot = &merged
	return merged
}

// mergeBurn applies a burn snapshot's rate + state + today fields onto a
// usage snapshot, bumping seq + generated_at_utc.
func mergeBurn(s wire.UsageSnapshot, b burn.Snapshot, seq int, now time.Time) wire.UsageSnapshot {
	s.Seq = seq
	s.GeneratedAtUTC = wire.FormatTime(now)
	s.BurnRatePerMinute = b.RatePerMinute
	s.BurnState = string(b.State)
	s.TodayTotalTokens = b.TodayTotalTokens
	s.TodaySessions = b.TodaySessionsCount
	s.Status.ActivitySources = append([]string(nil), b.ActivitySources...)
	if b.HasObserved && s.Status.DataSource == wire.DataSourceAPIOnly {
		s.Status.DataSource = wire.DataSourceAPIAndJSONL
	}
	return s
}

// latestInner returns the most recent snapshot merged with the live burn
// rate. If none has been fetched yet, returns a degraded snapshot in
// networkError state.
func (s *State) latestInner(now time.Time) wire.UsageSnapshot {
	burnSnap := s.burn.Snapshot(now)
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.latestSnapshot != nil {
		merged := mergeBurnInPlace(*s.latestSnapshot, burnSnap)
		return merged
	}
	snap := wire.Degraded(s.provider, s.seq, s.producer, now, wire.StateNetworkError)
	s.latestSnapshot = &snap
	state := snap.Status.State
	s.lastState = &state
	return mergeBurnInPlace(snap, burnSnap)
}

// mergeBurnInPlace returns a copy with burn fields applied (no seq bump).
func mergeBurnInPlace(s wire.UsageSnapshot, b burn.Snapshot) wire.UsageSnapshot {
	s.BurnRatePerMinute = b.RatePerMinute
	s.BurnState = string(b.State)
	s.TodayTotalTokens = b.TodayTotalTokens
	s.TodaySessions = b.TodaySessionsCount
	s.Status.ActivitySources = append([]string(nil), b.ActivitySources...)
	if b.HasObserved && s.Status.DataSource == wire.DataSourceAPIOnly {
		s.Status.DataSource = wire.DataSourceAPIAndJSONL
	}
	return s
}

// refreshInner decides whether to issue a real upstream fetch and returns
// the resulting UsageUpdate. Mirrors UsageState.refreshSnapshot exactly:
//
//   - local snapshotter hit (for example codex-lb) → use it before credential I/O
//   - cache hit (same account, < cacheTTL since last fetch) → bump seq,
//     re-merge burn rate, return cached
//   - rate-limit suspended (recent 429) → same as cache hit
//   - else fetch from upstream; on 401, refresh credential + retry once
//   - on transient error (network/server), serve sticky last-good for up
//     to stickyTTL
//   - on auth/credential errors, return degraded immediately
func (s *State) refreshInner(ctx context.Context, now time.Time) UsageUpdate {
	burnSnap := s.burn.Snapshot(now)

	if update, ok := s.tryLocalSnapshot(ctx, now, burnSnap); ok {
		return update
	}

	currentAccount := s.credentials.CurrentAccountKey(ctx, s.provider)

	s.mu.Lock()
	isSameAccount := currentAccount != "" && s.cacheAccountKey == currentAccount
	cacheValid := isSameAccount && !s.lastFetchAt.IsZero() && now.Sub(s.lastFetchAt) < s.cacheTTL
	suspended := currentAccount != "" && s.fetchSuspendedAccountKey == currentAccount &&
		!s.fetchSuspendedUntil.IsZero() && now.Before(s.fetchSuspendedUntil)
	if !suspended && !s.fetchSuspendedUntil.IsZero() {
		// Expired suspensions and account switches must not leak a previous
		// account's retry deadline into a new snapshot.
		s.fetchSuspendedUntil = time.Time{}
		s.fetchSuspendedAccountKey = ""
		cacheValid = false
	}

	if cacheValid && s.latestSnapshot != nil {
		s.seq++
		merged := mergeBurn(*s.latestSnapshot, burnSnap, s.seq, now)
		s.latestSnapshot = &merged
		s.mu.Unlock()
		return UsageUpdate{Snapshot: merged}
	}
	if suspended {
		s.seq++
		seq := s.seq
		if s.lastOkSnapshot != nil && s.lastOkAccountKey == currentAccount &&
			!s.lastOkAt.IsZero() && now.Sub(s.lastOkAt) < s.stickyTTL {
			merged := mergeBurn(*s.lastOkSnapshot, burnSnap, seq, now)
			merged.Status.Stale = true
			merged.Status.RetryAt = retryAtPointer(s.fetchSuspendedUntil, now)
			s.latestSnapshot = &merged
			s.mu.Unlock()
			return UsageUpdate{Snapshot: merged}
		}
		raw := wire.Degraded(s.provider, seq, s.producer, now, wire.StateRateLimited)
		raw.Status.RetryAt = retryAtPointer(s.fetchSuspendedUntil, now)
		merged := mergeBurnInPlace(raw, burnSnap)
		s.latestSnapshot = &merged
		limited := wire.StateRateLimited
		s.lastState = &limited
		s.mu.Unlock()
		return UsageUpdate{Snapshot: merged}
	}

	s.seq++
	seq := s.seq
	s.lastFetchAt = now
	s.lastQuotaAttemptAt = now
	s.mu.Unlock()

	credential, err := s.credentials.Load(ctx, s.provider)
	if err != nil {
		state := mapCredentialError(s.provider, err)
		return s.applyError(seq, now, currentAccount, state, err, credentialErrorKind(err), false, burnSnap)
	}

	raw, fetchErr := s.usageClient.Snapshot(ctx, s.provider, credential, seq, now)
	unresolvedAuthRejection := false
	if fetchErr != nil && usage.IsUnauthorized(fetchErr) {
		s.logger.Info("upstream 401 — entering oauth recovery",
			"provider", s.provider,
			"seq", seq,
			"consecutive_auth_expired", s.ConsecutiveAuthExpired())
		raw, fetchErr, unresolvedAuthRejection = s.recoverUnauthorized(ctx, credential, seq, now)
	}

	if fetchErr != nil {
		state := mapRecoveryError(s.provider, fetchErr)
		return s.applyError(seq, now, currentAccount, state, fetchErr, refreshErrorKind(fetchErr), unresolvedAuthRejection, burnSnap)
	}

	// Successful fetch — update both the live snapshot and the sticky
	// last-good cache. Account-key both so a transient error right
	// after an account switch can't resurface the old account's quota.
	if raw.Status.QuotaObservedAt == nil {
		observed := raw.GeneratedAtUTC
		raw.Status.QuotaObservedAt = &observed
	}
	merged := mergeBurnInPlace(raw, burnSnap)
	s.mu.Lock()
	if s.seq != seq && s.latestSnapshot != nil {
		latest := *s.latestSnapshot
		s.mu.Unlock()
		return UsageUpdate{Snapshot: latest}
	}
	s.latestSnapshot = &merged
	rawCopy := raw
	s.lastOkSnapshot = &rawCopy
	s.lastOkAt = now
	s.cacheAccountKey = currentAccount
	s.lastOkAccountKey = currentAccount
	s.fetchSuspendedUntil = time.Time{}
	s.fetchSuspendedAccountKey = ""
	s.lastQuotaSuccessAt = now
	state := raw.Status.State
	s.lastState = &state
	// Any successful response breaks a run of unresolved auth rejections.
	// Only confirmed, back-to-back auth failures may drive the guard.
	s.consecutiveAuthExpired = 0
	s.mu.Unlock()
	return UsageUpdate{Snapshot: merged}
}

// recoverUnauthorized performs both recovery stages for a confirmed upstream
// 401/403. A newer token found on disk is tried first. If that token is also
// rejected, the OAuth refresher still gets a chance; a non-auth failure at
// either stage is returned as such and must not advance the restart guard.
func (s *State) recoverUnauthorized(ctx context.Context, credential auth.OAuthCredential, seq int, now time.Time) (wire.UsageSnapshot, error, bool) {
	latest, reloadErr := s.credentials.Reload(ctx, s.provider)
	if reloadErr != nil {
		s.logger.Warn("oauth recovery: disk reload failed",
			"provider", s.provider,
			"err", reloadErr)
	} else if latest.AccessToken == credential.AccessToken {
		s.logger.Info("oauth recovery: disk token unchanged — will invoke refresher",
			"provider", s.provider)
	} else {
		s.logger.Info("oauth recovery: disk had newer token — retrying before refresh",
			"provider", s.provider)
		credential = latest
		raw, retryErr := s.usageClient.Snapshot(ctx, s.provider, credential, seq, now)
		if retryErr == nil {
			s.logger.Info("oauth recovery: retry-with-disk-token succeeded",
				"provider", s.provider)
			return raw, nil, false
		}
		s.logger.Warn("oauth recovery: retry-with-disk-token failed",
			"provider", s.provider,
			"err", retryErr)
		if !usage.IsUnauthorized(retryErr) {
			return wire.UsageSnapshot{}, retryErr, false
		}
	}

	if s.refresher == nil {
		err := &auth.RefreshError{Kind: auth.RefreshKindNoRefreshToken}
		s.logger.Warn("oauth recovery: no refresher configured", "provider", s.provider)
		return wire.UsageSnapshot{}, err, true
	}

	s.logger.Info("oauth recovery: calling refresher.Refresh",
		"provider", s.provider,
		"refresh_token_present", credential.RefreshToken != "")
	refreshed, refreshErr := s.refresher.Refresh(ctx, credential)
	if refreshErr != nil {
		s.logger.Warn("oauth recovery: refresher.Refresh failed",
			"provider", s.provider,
			"err", refreshErr)
		return wire.UsageSnapshot{}, refreshErr, isDefinitiveAuthRecoveryFailure(refreshErr)
	}

	s.logger.Info("oauth recovery: refresher.Refresh succeeded — retrying",
		"provider", s.provider,
		"new_access_token_differs", refreshed.AccessToken != credential.AccessToken)
	raw, retryErr := s.usageClient.Snapshot(ctx, s.provider, refreshed, seq, now)
	if retryErr != nil {
		s.logger.Warn("oauth recovery: retry-after-refresh failed",
			"provider", s.provider,
			"err", retryErr)
		return wire.UsageSnapshot{}, retryErr, usage.IsUnauthorized(retryErr)
	}
	s.logger.Info("oauth recovery: retry-after-refresh succeeded", "provider", s.provider)
	return raw, nil, false
}

func (s *State) tryLocalSnapshot(ctx context.Context, now time.Time, burnSnap burn.Snapshot) (UsageUpdate, bool) {
	s.mu.Lock()
	localUsage := s.localUsage
	if localUsage == nil {
		s.mu.Unlock()
		return UsageUpdate{}, false
	}
	s.seq++
	seq := s.seq
	s.mu.Unlock()

	raw, ok := localUsage.Snapshot(ctx, seq, now)
	if !ok {
		s.mu.Lock()
		if s.seq == seq {
			s.seq--
		}
		s.mu.Unlock()
		return UsageUpdate{}, false
	}

	if raw.Status.State == wire.StateOK && !raw.Status.Stale && raw.Status.QuotaObservedAt == nil {
		observed := raw.GeneratedAtUTC
		raw.Status.QuotaObservedAt = &observed
	}
	merged := mergeBurnInPlace(raw, burnSnap)
	s.mu.Lock()
	if s.seq != seq && s.latestSnapshot != nil {
		latest := *s.latestSnapshot
		s.mu.Unlock()
		return UsageUpdate{Snapshot: latest}, true
	}
	s.latestSnapshot = &merged
	s.cacheAccountKey = "local"
	s.lastQuotaAttemptAt = now
	if raw.Status.State == wire.StateOK && !raw.Status.Stale {
		rawCopy := raw
		s.lastOkSnapshot = &rawCopy
		s.lastOkAt = now
		s.lastOkAccountKey = "local"
		s.lastQuotaSuccessAt = now
		s.lastQuotaErrorKind = ""
	} else {
		s.recordRefreshFailureLocked(now, "local_quota_unavailable", false)
	}
	state := raw.Status.State
	s.lastState = &state
	s.consecutiveAuthExpired = 0
	s.mu.Unlock()
	return UsageUpdate{Snapshot: merged}, true
}

// ConsecutiveAuthExpired returns the count of back-to-back, unresolved
// upstream auth rejections. The historical name is retained for callers.
func (s *State) ConsecutiveAuthExpired() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.consecutiveAuthExpired
}

// Diagnostics returns a stable, privacy-safe provider summary without
// triggering credential I/O or an upstream refresh.
func (s *State) Diagnostics() Diagnostics {
	s.mu.Lock()
	defer s.mu.Unlock()
	return Diagnostics{
		Observed:                  s.latestSnapshot != nil,
		State:                     copyState(s.lastState),
		LastQuotaAttemptAt:        formattedTimePtr(s.lastQuotaAttemptAt),
		LastQuotaSuccessAt:        formattedTimePtr(s.lastQuotaSuccessAt),
		LastQuotaErrorAt:          formattedTimePtr(s.lastQuotaErrorAt),
		LastQuotaErrorKind:        s.lastQuotaErrorKind,
		ConsecutiveAuthRejections: s.consecutiveAuthExpired,
	}
}

// applyError handles the post-fetch failure path: 429 backoff, sticky
// last-good fallback for transient errors, and degraded snapshot otherwise.
func (s *State) applyError(seq int, now time.Time, currentAccount string, state wire.ProviderState, originalErr error, errorKind string, unresolvedAuthRejection bool, burnSnap burn.Snapshot) UsageUpdate {
	s.logger.Warn("usage refresh failed",
		"provider", s.provider,
		"state", state,
		"err", originalErr)

	s.mu.Lock()
	if s.seq != seq && s.latestSnapshot != nil {
		latest := *s.latestSnapshot
		s.mu.Unlock()
		return UsageUpdate{Snapshot: latest}
	}
	s.mu.Unlock()

	// 429 → freeze our own fetches so cron-period refreshes don't keep
	// re-hitting the same Cloudflare bucket.
	var retryAt *string
	if errorStatus(originalErr) == 429 {
		delay := rateLimitDelay(originalErr, s.rateLimitBackoff)
		until := now.Add(delay)
		retryAt = retryAtPointer(until, now)
		s.mu.Lock()
		s.fetchSuspendedUntil = until
		s.fetchSuspendedAccountKey = currentAccount
		s.mu.Unlock()
		s.logger.Warn("upstream 429 — suspending fetches",
			"provider", s.provider,
			"until", wire.FormatTime(until))
	}

	// Sticky last-good: only for transient (network/server) errors,
	// only when we have a recent OK snapshot from the SAME account.
	isTransient := state == wire.StateNetworkError || state == wire.StateRateLimited
	s.mu.Lock()
	s.recordRefreshFailureLocked(now, errorKind, unresolvedAuthRejection)
	if isTransient && currentAccount != "" && s.lastOkAccountKey == currentAccount &&
		s.lastOkSnapshot != nil && !s.lastOkAt.IsZero() && now.Sub(s.lastOkAt) < s.stickyTTL {
		merged := mergeBurn(*s.lastOkSnapshot, burnSnap, seq, now)
		merged.Status.Stale = true
		merged.Status.RetryAt = retryAt
		s.latestSnapshot = &merged
		ok := wire.StateOK
		s.lastState = &ok
		s.mu.Unlock()
		return UsageUpdate{Snapshot: merged}
	}
	s.mu.Unlock()

	raw := wire.Degraded(s.provider, seq, s.producer, now, state)
	raw.Status.RetryAt = retryAt
	merged := mergeBurnInPlace(raw, burnSnap)
	s.mu.Lock()
	s.latestSnapshot = &merged
	s.cacheAccountKey = currentAccount
	s.lastState = &state
	s.mu.Unlock()
	return UsageUpdate{Snapshot: merged}
}

func (s *State) recordRefreshFailureLocked(now time.Time, errorKind string, unresolvedAuthRejection bool) {
	s.lastQuotaErrorAt = now
	s.lastQuotaErrorKind = errorKind
	if unresolvedAuthRejection {
		s.consecutiveAuthExpired++
		return
	}
	// Consecutive means exactly that: missing credentials, network failures,
	// rate limits, 5xx responses, and contract errors all break the run.
	s.consecutiveAuthExpired = 0
}

const maximumRateLimitDelay = 24 * time.Hour

// rateLimitDelay extracts Retry-After from either usage or OAuth-refresh
// failures. Positive delays are capped at 24 hours; absent and malformed
// values use the configured fallback.
func rateLimitDelay(err error, fallback time.Duration) time.Duration {
	if retryAfter, ok := usage.RetryAfter(err); ok && retryAfter > 0 {
		return min(retryAfter, maximumRateLimitDelay)
	}
	var refreshErr *auth.RefreshError
	if errors.As(err, &refreshErr) && refreshErr.Status == 429 &&
		refreshErr.RetryAfter > 0 {
		return min(refreshErr.RetryAfter, maximumRateLimitDelay)
	}
	return fallback
}

func formattedTimePtr(t time.Time) *string {
	if t.IsZero() {
		return nil
	}
	formatted := wire.FormatTime(t)
	return &formatted
}

func retryAtPointer(deadline, generatedAt time.Time) *string {
	if deadline.IsZero() || !generatedAt.Before(deadline) {
		return nil
	}
	retryAt := wire.FormatTime(deadline)
	if retryAt <= wire.FormatTime(generatedAt) {
		// Millisecond wire precision can collapse a very short positive delay
		// to the generated timestamp. Omit it instead of emitting a deadline
		// that downstream validators must reject as already elapsed.
		return nil
	}
	return &retryAt
}

func copyState(p *wire.ProviderState) *wire.ProviderState {
	if p == nil {
		return nil
	}
	v := *p
	return &v
}

func mapCredentialError(provider wire.Provider, err error) wire.ProviderState {
	if auth.IsNotFound(err) {
		if provider == wire.ProviderCodex {
			return wire.StateCodexLoggedOut
		}
		return wire.StateAuthExpired
	}
	var ce auth.CredentialFileError
	if errors.As(err, &ce) {
		switch ce.Kind {
		case "missing_token":
			if provider == wire.ProviderCodex {
				return wire.StateCodexLoggedOut
			}
			return wire.StateAuthExpired
		case "invalid_json":
			return wire.StateQuotaEndpointChanged
		}
	}
	return wire.StateNetworkError
}

func mapRecoveryError(provider wire.Provider, err error) wire.ProviderState {
	if usage.IsUnauthorized(err) {
		if provider == wire.ProviderCodex {
			return wire.StateCodexLoggedOut
		}
		return wire.StateAuthExpired
	}
	if usage.ServerStatus(err) == 429 {
		return wire.StateRateLimited
	}
	if usage.IsServer(err) {
		return wire.StateNetworkError
	}
	var ae *usage.APIError
	if errors.As(err, &ae) && ae.Kind == usage.KindInvalidResponse {
		return wire.StateQuotaEndpointChanged
	}
	var re *auth.RefreshError
	if errors.As(err, &re) {
		switch re.Kind {
		case auth.RefreshKindNoRefreshToken, auth.RefreshKindCodexLoginRequired:
			if provider == wire.ProviderCodex {
				return wire.StateCodexLoggedOut
			}
			return wire.StateAuthExpired
		case auth.RefreshKindInvalidResponse:
			return wire.StateQuotaEndpointChanged
		case auth.RefreshKindRejected:
			if isDefinitiveAuthRecoveryFailure(err) {
				if provider == wire.ProviderCodex {
					return wire.StateCodexLoggedOut
				}
				return wire.StateAuthExpired
			}
			if re.Status == 429 {
				return wire.StateRateLimited
			}
			if re.Status == 408 || re.Status == 425 || re.Status >= 500 {
				return wire.StateNetworkError
			}
			return wire.StateQuotaEndpointChanged
		case auth.RefreshKindNetwork:
			return wire.StateNetworkError
		case auth.RefreshKindTransient:
			if re.Status == 429 {
				return wire.StateRateLimited
			}
			return wire.StateNetworkError
		case auth.RefreshKindPersistence:
			return wire.StateNetworkError
		}
	}
	return wire.StateNetworkError
}

func isDefinitiveAuthRecoveryFailure(err error) bool {
	if usage.IsUnauthorized(err) {
		return true
	}
	var re *auth.RefreshError
	if !errors.As(err, &re) {
		return false
	}
	switch re.Kind {
	case auth.RefreshKindNoRefreshToken, auth.RefreshKindCodexLoginRequired:
		return true
	case auth.RefreshKindRejected:
		if re.Status == 401 || re.Status == 403 {
			return true
		}
		if re.Status != 400 {
			return false
		}
		// A generic 400 can mean the refresh endpoint contract changed. Count
		// only explicit OAuth credential rejection markers as auth failure.
		message := strings.ToLower(re.Message)
		for _, marker := range []string{
			"invalid_grant",
			"invalid grant",
			"invalid_token",
			"invalid token",
			"invalid refresh token",
			"refresh token is invalid",
			"login_required",
			"token expired",
			"unauthorized",
		} {
			if strings.Contains(message, marker) {
				return true
			}
		}
		return false
	default:
		return false
	}
}

func credentialErrorKind(err error) string {
	var ce auth.CredentialFileError
	if errors.As(err, &ce) {
		switch ce.Kind {
		case "not_found", "missing_token":
			return "credential_missing"
		case "invalid_json":
			return "credential_contract"
		}
	}
	return "credential_io"
}

func refreshErrorKind(err error) string {
	if usage.IsUnauthorized(err) {
		return "auth_rejected"
	}
	var ae *usage.APIError
	if errors.As(err, &ae) {
		switch ae.Kind {
		case usage.KindInvalidResponse:
			return "upstream_contract"
		case usage.KindServer:
			if ae.Status == 429 {
				return "rate_limited"
			}
			return "upstream_server"
		case usage.KindNetwork:
			return "network"
		}
	}
	var re *auth.RefreshError
	if errors.As(err, &re) {
		switch re.Kind {
		case auth.RefreshKindNoRefreshToken, auth.RefreshKindCodexLoginRequired:
			return "auth_rejected"
		case auth.RefreshKindInvalidResponse:
			return "refresh_contract"
		case auth.RefreshKindRejected:
			if isDefinitiveAuthRecoveryFailure(err) {
				return "auth_rejected"
			}
			if re.Status == 429 {
				return "rate_limited"
			}
			if re.Status == 408 || re.Status == 425 || re.Status >= 500 {
				return "refresh_server"
			}
			return "refresh_contract"
		case auth.RefreshKindNetwork:
			return "network"
		case auth.RefreshKindTransient:
			if re.Status == 429 {
				return "rate_limited"
			}
			return "refresh_server"
		case auth.RefreshKindPersistence:
			return "credential_persist"
		}
	}
	return "unknown"
}

func errorStatus(err error) int {
	if status := usage.ServerStatus(err); status != 0 {
		return status
	}
	var re *auth.RefreshError
	if errors.As(err, &re) && (re.Kind == auth.RefreshKindRejected || re.Kind == auth.RefreshKindTransient) {
		return re.Status
	}
	return 0
}

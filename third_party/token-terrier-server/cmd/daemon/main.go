// Command daemon serves local token usage data over HTTP/SSE.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/pprof"
	"os"
	"os/signal"
	"path/filepath"
	"runtime/debug"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/api"
	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/burn"
	"github.com/codemoo/token-terrier/server-go/internal/claudeswap"
	"github.com/codemoo/token-terrier/server-go/internal/codexaccounts"
	"github.com/codemoo/token-terrier/server-go/internal/codexlb"
	"github.com/codemoo/token-terrier/server-go/internal/hermes"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/sse"
	"github.com/codemoo/token-terrier/server-go/internal/state"
	"github.com/codemoo/token-terrier/server-go/internal/usage"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

const (
	periodicRefreshInterval = 60 * time.Second
	maximumEmbeddedConfig   = 64 << 10
	// authFailureThreshold caps how many consecutive auth-expired refreshes
	// the daemon tolerates before exiting for its supervisor to restart it. With a
	// 60s refresh ticker that's roughly N minutes of being stuck. Deliberately
	// generous so a real user-initiated logout doesn't cause a thrash.
	authFailureThreshold = 30
	// authFailureCheckInterval cadence at which the guard polls per-provider
	// counters. Independent of refresh ticker so it fires even if refresh
	// itself ever wedges.
	authFailureCheckInterval = time.Minute
)

// These values are intentionally strings so release builds can inject them:
//
//	go build -ldflags "-X main.serverVersion=1.2.3 -X main.serverGitSHA=<sha> -X main.serverGitDirty=false -X main.serverBuildTime=<rfc3339>" ./cmd/daemon
//
// Go VCS build settings fill SHA/dirty when available; build time remains
// null unless the build pipeline explicitly supplies it.
var (
	serverVersion   = "0.0.0-dev"
	serverGitSHA    string
	serverGitDirty  string
	serverBuildTime string
)

func main() {
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelInfo}))
	signalCtx, stopSignals := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stopSignals()
	ctx, stopParentWatch, err := managedParentContext(signalCtx, os.Getppid)
	if err != nil {
		logger.Error("invalid managed parent", "err", err)
		os.Exit(1)
	}
	defer stopParentWatch()
	configuration := runtimeConfiguration{}
	if os.Getenv("HMUX_USAGE_EMBEDDED") == "1" {
		var leaseReader *bufio.Reader
		configuration, leaseReader, err = readEmbeddedConfiguration(os.Stdin)
		if err != nil {
			logger.Error("invalid embedded configuration", "err", err)
			os.Exit(1)
		}
		var stopLeaseWatch context.CancelFunc
		ctx, stopLeaseWatch = embeddedLeaseContext(ctx, leaseReader)
		defer stopLeaseWatch()
	}
	if err := runConfigured(ctx, logger, configuration); err != nil {
		logger.Error("server stopped with error", "err", err)
		os.Exit(1)
	}
}

type embeddedConfiguration struct {
	Schema         int    `json:"schema"`
	ClaudeToken    string `json:"claude_token"`
	CodexToken     string `json:"codex_token"`
	CodexLBAPIKey  string `json:"codex_lb_api_key,omitempty"`
	CodexLBBaseURL string `json:"codex_lb_base_url,omitempty"`
}

type embeddedBootstrap struct {
	Schema int    `json:"schema"`
	PID    int    `json:"pid"`
	Host   string `json:"host"`
	Port   int    `json:"port"`
}

type runtimeConfiguration struct {
	embedded       bool
	tokens         wire.BearerTokens
	codexLBAPIKey  string
	codexLBBaseURL string
	bootstrap      io.Writer
}

var errEmbeddedCredentialWriteDisabled = errors.New("embedded HMux usage runtime cannot write provider credentials")

// embeddedCredentialSource keeps account-switch detection and bounded reads,
// but makes credential mutation impossible even if a future caller
// accidentally wires a refresher into embedded mode.
type embeddedCredentialSource struct {
	source *auth.LocalSource
}

func (s *embeddedCredentialSource) Read(ctx context.Context, provider wire.Provider) ([]byte, error) {
	return s.source.Read(ctx, provider)
}

func (s *embeddedCredentialSource) Revision(ctx context.Context, provider wire.Provider) (auth.SourceRevision, error) {
	return s.source.Revision(ctx, provider)
}

func (s *embeddedCredentialSource) Write(context.Context, wire.Provider, []byte) error {
	return errEmbeddedCredentialWriteDisabled
}

func configuredRefresher(configuration runtimeConfiguration, store *auth.CredentialStore) state.Refresher {
	if configuration.embedded {
		return nil
	}
	return auth.NewRefresher(store)
}

func readEmbeddedConfiguration(input io.Reader) (runtimeConfiguration, *bufio.Reader, error) {
	reader := bufio.NewReaderSize(input, maximumEmbeddedConfig+1)
	line, err := reader.ReadSlice('\n')
	if err != nil {
		return runtimeConfiguration{}, nil, fmt.Errorf("read bootstrap line: %w", err)
	}
	if len(line) == 0 || len(line) > maximumEmbeddedConfig {
		return runtimeConfiguration{}, nil, errors.New("embedded configuration exceeds size limit")
	}
	decoder := json.NewDecoder(strings.NewReader(string(line)))
	decoder.DisallowUnknownFields()
	var value embeddedConfiguration
	if err := decoder.Decode(&value); err != nil {
		return runtimeConfiguration{}, nil, fmt.Errorf("decode bootstrap line: %w", err)
	}
	if value.Schema != 1 {
		return runtimeConfiguration{}, nil, errors.New("unsupported embedded configuration schema")
	}
	tokens := wire.BearerTokens{Claude: value.ClaudeToken, Codex: value.CodexToken}
	if err := validateEmbeddedTokens(tokens); err != nil {
		return runtimeConfiguration{}, nil, err
	}
	if len(value.CodexLBAPIKey) > 4096 || len(value.CodexLBBaseURL) > 2048 {
		return runtimeConfiguration{}, nil, errors.New("embedded codex-lb configuration exceeds size limit")
	}
	return runtimeConfiguration{
		embedded:       true,
		tokens:         tokens,
		codexLBAPIKey:  strings.TrimSpace(value.CodexLBAPIKey),
		codexLBBaseURL: strings.TrimSpace(value.CodexLBBaseURL),
	}, reader, nil
}

func validateEmbeddedTokens(tokens wire.BearerTokens) error {
	valid := func(value string) bool {
		if len(value) < 32 || len(value) > 4096 {
			return false
		}
		for _, character := range value {
			if character < 0x21 || character > 0x7e {
				return false
			}
		}
		return true
	}
	if !valid(tokens.Claude) || !valid(tokens.Codex) || tokens.Claude == tokens.Codex {
		return errors.New("invalid embedded bearer tokens")
	}
	return nil
}

func embeddedLeaseContext(parent context.Context, lease io.Reader) (context.Context, context.CancelFunc) {
	ctx, cancel := context.WithCancel(parent)
	go func() {
		_, _ = io.Copy(io.Discard, lease)
		cancel()
	}()
	return ctx, cancel
}

// managedParentContext makes the bundled HMux helper disappear when its app
// parent exits, including crash/kill paths where Swift cannot send SIGTERM.
// Standalone upstream behavior is unchanged when HMUX_USAGE_PARENT_PID is not
// present.
func managedParentContext(parent context.Context, getParentPID func() int) (context.Context, context.CancelFunc, error) {
	ctx, cancel := context.WithCancel(parent)
	raw := strings.TrimSpace(os.Getenv("HMUX_USAGE_PARENT_PID"))
	if raw == "" {
		return ctx, cancel, nil
	}
	expected, err := strconv.Atoi(raw)
	if err != nil || expected < 2 || expected != getParentPID() {
		cancel()
		return nil, func() {}, errors.New("HMUX_USAGE_PARENT_PID does not match the direct parent")
	}
	go func() {
		ticker := time.NewTicker(2 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if getParentPID() != expected {
					cancel()
					return
				}
			}
		}
	}()
	return ctx, cancel, nil
}

func run(parentCtx context.Context, logger *slog.Logger) error {
	return runConfigured(parentCtx, logger, runtimeConfiguration{})
}

func runConfigured(parentCtx context.Context, logger *slog.Logger, configuration runtimeConfiguration) error {
	tokens := configuration.tokens
	if !configuration.embedded {
		var err error
		tokens, err = loadBearerTokens(logger)
		if err != nil {
			return fmt.Errorf("bearer token store: %w", err)
		}
	}

	producer := wire.CurrentProducer()
	rootCtx, cancelRoot := context.WithCancelCause(parentCtx)
	defer cancelRoot(nil)
	claudeEnabled := os.Getenv("TOKEN_USAGE_DISABLE_CLAUDE") != "1"
	codexEnabled := os.Getenv("TOKEN_USAGE_DISABLE_CODEX") != "1"

	home, _ := os.UserHomeDir()
	claudeCred := strings.TrimSpace(os.Getenv("TOKEN_USAGE_CLAUDE_CRED"))
	if claudeCred == "" {
		claudeCred = filepath.Join(home, ".claude", ".credentials.json")
	}
	codexCred := strings.TrimSpace(os.Getenv("TOKEN_USAGE_CODEX_CRED"))
	if codexCred == "" {
		codexCred = filepath.Join(home, ".codex", "auth.json")
	}
	localCredSource := &auth.LocalSource{
		ClaudePath: claudeCred,
		CodexPath:  codexCred,
	}
	var credSource auth.ReadSource = localCredSource
	if configuration.embedded {
		credSource = &embeddedCredentialSource{source: localCredSource}
		logger.Info("credential source: local read-only filesystem")
	} else {
		logger.Info("credential source: local filesystem")
	}
	credStore := auth.NewCredentialStore(credSource)
	usageClient := usage.NewClient(producer)
	refresher := configuredRefresher(configuration, credStore)

	// One BurnTracker per provider — they're independent (different sliding
	// windows for different providers' event streams).
	now := time.Now()
	claudeBurn := burn.New(time.Local, now)
	codexBurn := burn.New(time.Local, now)

	claudeState := state.New(wire.ProviderClaude, credStore, usageClient, refresher, claudeBurn, producer, logger)
	codexState := state.New(wire.ProviderCodex, credStore, usageClient, refresher, codexBurn, producer, logger)
	codexLBSnapshotter := codexlb.NewSnapshotterWithConfiguration(
		producer,
		logger,
		configuration.codexLBBaseURL,
		configuration.codexLBAPIKey,
	)
	codexState.SetLocalSnapshotter(codexLBSnapshotter)
	claudeSwapEnabled := claudeEnabled && os.Getenv("TOKEN_USAGE_DISABLE_CLAUDE_SWAP") != "1"
	var claudeSwapActivity *claudeswap.ActivityTracker
	var claudeSwapReader *claudeswap.Reader
	if claudeSwapEnabled {
		swapPath := strings.TrimSpace(os.Getenv("TOKEN_USAGE_CLAUDE_SWAP_ACCOUNTS"))
		if swapPath == "" {
			swapPath = filepath.Join(home, ".config", "token-usage", "claude-swap-accounts.json")
		}
		claudeSwapActivity = claudeswap.NewActivityTracker(time.Local, now)
		claudeSwapReader = claudeswap.NewReader(swapPath, logger)
		claudeSwapReader.SetActivityProvider(claudeSwapActivity)
		claudeState.SetAccountsProvider(claudeSwapReader)
	}
	codexAccountsEnabled := codexEnabled && os.Getenv("TOKEN_USAGE_DISABLE_CODEX_ACCOUNTS") != "1"
	var codexAccountsReader *codexaccounts.Reader
	if codexAccountsEnabled {
		p := strings.TrimSpace(os.Getenv("TOKEN_USAGE_CODEX_ACCOUNTS"))
		if p == "" {
			p = filepath.Join(home, ".config", "token-usage", "codex-lb-accounts.json")
		}
		codexAccountsReader = codexaccounts.NewReader(p, logger)
		codexState.SetAccountsProvider(codexAccountsReader)
	}
	claudeHub := sse.NewHub()
	codexHub := sse.NewHub()
	hermesEnabled := os.Getenv("TOKEN_USAGE_DISABLE_HERMES") != "1"
	var hermesPoller *hermes.Poller
	if hermesEnabled {
		hermesPoller = hermes.NewPoller(nil, logger)
	}
	jsonlEnabled := os.Getenv("TOKEN_USAGE_DISABLE_JSONL") != "1"
	var jsonlPoller *jsonl.Poller
	if jsonlEnabled {
		jsonlPoller = jsonl.NewPoller(nil, logger)
	}
	if hermesPoller != nil {
		hermesPoller.SetJSONLHealthy(func(provider wire.Provider) bool {
			if jsonlPoller == nil {
				return false
			}
			status := jsonlPoller.Status(provider)
			return status.Observed && status.State == "ok"
		})
	}
	srv := api.New(
		tokens,
		producer,
		claudeState,
		codexState,
		claudeHub,
		codexHub,
		logger,
		api.WithRootContext(rootCtx),
		api.WithBuildInfo(currentBuildInfo()),
		api.WithProviderEnabled(wire.ProviderClaude, claudeEnabled),
		api.WithProviderEnabled(wire.ProviderCodex, codexEnabled),
		api.WithOptionalSource(wire.ProviderClaude, "claude_swap_accounts", func() api.OptionalSourceDiagnostics {
			return claudeSwapDiagnostics(claudeSwapEnabled, claudeSwapReader)
		}),
		api.WithOptionalSource(wire.ProviderCodex, "codex_lb_accounts", func() api.OptionalSourceDiagnostics {
			return codexAccountsDiagnostics(codexAccountsEnabled, codexAccountsReader)
		}),
		api.WithOptionalSource(wire.ProviderCodex, "codex_lb_aggregate", func() api.OptionalSourceDiagnostics {
			return codexLBDiagnostics(codexLBSnapshotter)
		}),
		api.WithOptionalSource(wire.ProviderClaude, "hermes", func() api.OptionalSourceDiagnostics {
			return hermesDiagnostics(hermesEnabled, hermesPoller)
		}),
		api.WithOptionalSource(wire.ProviderCodex, "hermes", func() api.OptionalSourceDiagnostics {
			return hermesDiagnostics(hermesEnabled, hermesPoller)
		}),
		api.WithOptionalSource(wire.ProviderClaude, "jsonl", func() api.OptionalSourceDiagnostics {
			return jsonlDiagnostics(jsonlEnabled, jsonlPoller, wire.ProviderClaude)
		}),
		api.WithOptionalSource(wire.ProviderCodex, "jsonl", func() api.OptionalSourceDiagnostics {
			return jsonlDiagnostics(jsonlEnabled, jsonlPoller, wire.ProviderCodex)
		}),
	)

	host := strings.TrimSpace(os.Getenv("TOKEN_USAGE_BIND"))
	if host == "" {
		host = "127.0.0.1"
	}
	if err := validateHTTPBind(host, os.Getenv("TOKEN_USAGE_ALLOW_INSECURE_HTTP") == "1"); err != nil {
		return err
	}
	if !isLoopbackHost(host) {
		logger.Warn("insecure non-loopback HTTP explicitly enabled; use external TLS termination", "bind", host)
	}
	port := 18910
	if configuration.embedded {
		port = 0
	}
	if v := os.Getenv("TOKEN_USAGE_PORT"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n >= 0 && n < 65536 {
			port = n
		}
	}
	if configuration.embedded {
		host = "127.0.0.1"
		port = 0
	}
	addr := fmt.Sprintf("%s:%d", host, port)
	listener, err := net.Listen("tcp4", addr)
	if err != nil {
		return fmt.Errorf("http listen: %w", err)
	}
	defer listener.Close()
	tcpAddress, ok := listener.Addr().(*net.TCPAddr)
	if !ok || tcpAddress.IP == nil || !tcpAddress.IP.IsLoopback() || tcpAddress.Port < 1 || tcpAddress.Port > 65535 {
		return errors.New("http listener did not resolve to an exact loopback TCP address")
	}
	port = tcpAddress.Port

	httpServer := &http.Server{
		Addr:              addr,
		Handler:           srv.Routes(),
		ReadHeaderTimeout: 10 * time.Second,
		IdleTimeout:       75 * time.Second,
		MaxHeaderBytes:    16 << 10,
		BaseContext: func(net.Listener) context.Context {
			return rootCtx
		},
	}

	var wg sync.WaitGroup
	enabledProviders := map[wire.Provider]bool{
		wire.ProviderClaude: claudeEnabled,
		wire.ProviderCodex:  codexEnabled,
	}

	if jsonlEnabled {
		startJSONLPoller(rootCtx, &wg, jsonlPoller, claudeState, codexState, claudeHub, codexHub, logger, claudeSwapActivity, claudeSwapReader, enabledProviders)
	}
	// Hermes SQLite poller captures broader API usage when Hermes is present.
	// Set TOKEN_USAGE_DISABLE_HERMES=1 to skip it.
	if hermesEnabled {
		startHermesPoller(rootCtx, &wg, hermesPoller, claudeState, codexState, claudeHub, codexHub, logger, enabledProviders)
	}

	if claudeEnabled {
		startPeriodicRefresh(rootCtx, &wg, claudeState, claudeHub, wire.ProviderClaude, logger)
	}
	if codexEnabled {
		startPeriodicRefresh(rootCtx, &wg, codexState, codexHub, wire.ProviderCodex, logger)
	}
	startAuthFailureGuard(rootCtx, &wg, claudeState, codexState, enabledProviders, logger, func(err error) {
		cancelRoot(err)
	})
	profiler := startPprofListener(logger)

	if configuration.embedded {
		bootstrapWriter := configuration.bootstrap
		if bootstrapWriter == nil {
			bootstrapWriter = os.Stdout
		}
		if err := json.NewEncoder(bootstrapWriter).Encode(embeddedBootstrap{
			Schema: 1,
			PID:    os.Getpid(),
			Host:   "127.0.0.1",
			Port:   port,
		}); err != nil {
			return fmt.Errorf("write embedded bootstrap: %w", err)
		}
	}

	logger.Info("starting token-terrier server",
		"bind", host,
		"port", port,
		"producer_id", producer.ID,
		"producer_tz", producer.TimeZone,
		"claude_enabled", claudeEnabled,
		"codex_enabled", codexEnabled)

	serveDone := make(chan error, 1)
	go func() { serveDone <- httpServer.Serve(listener) }()
	serveResultRead := false
	var runErr error
	select {
	case <-rootCtx.Done():
		runErr = cancellationRunError(parentCtx, rootCtx)
	case serveErr := <-serveDone:
		serveResultRead = true
		if serveErr != nil && !errors.Is(serveErr, http.ErrServerClosed) {
			runErr = fmt.Errorf("http listen: %w", serveErr)
		}
	}

	shutdownErr := shutdownRuntime(
		func() { cancelRoot(runErr) },
		func() {
			srv.BeginShutdown()
			claudeHub.Close()
			codexHub.Close()
			claudeHub.Wait()
			codexHub.Wait()
		},
		func() {
			wg.Wait()
			srv.WaitBackground()
		},
		func() error {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			var errs []error
			if profiler != nil {
				if err := profiler.shutdown(shutdownCtx); err != nil {
					errs = append(errs, fmt.Errorf("pprof shutdown: %w", err))
				}
			}
			if err := httpServer.Shutdown(shutdownCtx); err != nil {
				errs = append(errs, fmt.Errorf("http shutdown: %w", err))
			}
			return errors.Join(errs...)
		},
	)
	if !serveResultRead {
		select {
		case serveErr := <-serveDone:
			if serveErr != nil && !errors.Is(serveErr, http.ErrServerClosed) && runErr == nil {
				runErr = fmt.Errorf("http listen: %w", serveErr)
			}
		default:
		}
	}
	logger.Info("stopped")
	return errors.Join(runErr, shutdownErr)
}

func cancellationRunError(parentCtx, rootCtx context.Context) error {
	// A signal or caller cancellation is a normal supervised shutdown. A
	// child-only cancellation cause (for example the confirmed auth guard)
	// remains an error so launchd can observe and restart it.
	if parentCtx.Err() != nil {
		return nil
	}
	cause := context.Cause(rootCtx)
	if cause == nil || errors.Is(cause, context.Canceled) || errors.Is(cause, context.DeadlineExceeded) {
		return nil
	}
	return cause
}

func validateHTTPBind(host string, insecureOptIn bool) error {
	if isLoopbackHost(host) || insecureOptIn {
		return nil
	}
	return fmt.Errorf(
		"refusing plaintext non-loopback bind %q; keep loopback or set TOKEN_USAGE_ALLOW_INSECURE_HTTP=1 behind TLS termination",
		host,
	)
}

func isLoopbackHost(host string) bool {
	normalized := strings.TrimSuffix(strings.ToLower(strings.Trim(strings.TrimSpace(host), "[]")), ".")
	if normalized == "localhost" {
		return true
	}
	ip := net.ParseIP(normalized)
	return ip != nil && ip.IsLoopback()
}

func currentBuildInfo() api.BuildInfo {
	version := strings.TrimSpace(serverVersion)
	sha := strings.TrimSpace(serverGitSHA)
	dirtyText := strings.TrimSpace(serverGitDirty)
	if build, ok := debug.ReadBuildInfo(); ok {
		if (version == "" || version == "0.0.0-dev") && build.Main.Version != "" && build.Main.Version != "(devel)" {
			version = strings.TrimPrefix(build.Main.Version, "v")
		}
		for _, setting := range build.Settings {
			switch setting.Key {
			case "vcs.revision":
				if sha == "" {
					sha = setting.Value
				}
			case "vcs.modified":
				if dirtyText == "" {
					dirtyText = setting.Value
				}
			}
		}
	}
	if version == "" {
		version = "0.0.0-dev"
	}
	if sha == "" {
		sha = "unknown"
	}
	var dirty *bool
	if parsed, err := strconv.ParseBool(dirtyText); err == nil {
		dirty = &parsed
	}
	var built *string
	if value := strings.TrimSpace(serverBuildTime); value != "" {
		built = &value
	}
	return api.BuildInfo{
		Name:         "token-terrier-server",
		Version:      version,
		GitSHA:       sha,
		Dirty:        dirty,
		BuildTime:    built,
		Schema:       api.SnapshotSchema,
		Capabilities: append([]string(nil), api.DefaultCapabilities...),
	}
}

func claudeSwapDiagnostics(enabled bool, reader *claudeswap.Reader) api.OptionalSourceDiagnostics {
	if !enabled || reader == nil {
		return api.OptionalSourceDiagnostics{Enabled: false, State: "disabled"}
	}
	status := reader.Status()
	state := string(status.State)
	if status.LastGoodExpired {
		state = "expired"
	}
	return api.OptionalSourceDiagnostics{
		Enabled:               true,
		Observed:              status.LastCheckedAt != nil,
		State:                 state,
		LastScanAt:            status.LastCheckedAt,
		LastSuccessAt:         status.LastSuccessAt,
		LastErrorAt:           status.LastErrorAt,
		LastErrorKind:         status.LastErrorKind,
		SourceUpdatedAt:       status.SourceUpdatedAt,
		UsingLastGood:         status.UsingLastGood,
		LastGoodExpired:       status.LastGoodExpired,
		AgeSeconds:            status.AgeSeconds,
		MaxLastGoodAgeSeconds: status.MaxLastGoodAgeSec,
	}
}

func codexAccountsDiagnostics(enabled bool, reader *codexaccounts.Reader) api.OptionalSourceDiagnostics {
	if !enabled || reader == nil {
		return api.OptionalSourceDiagnostics{Enabled: false, State: "disabled"}
	}
	status := reader.Status()
	state := string(status.State)
	if status.LastGoodExpired {
		state = "expired"
	}
	return api.OptionalSourceDiagnostics{
		Enabled:               true,
		Observed:              status.LastCheckedAt != nil,
		State:                 state,
		LastScanAt:            status.LastCheckedAt,
		LastSuccessAt:         status.LastSuccessAt,
		LastErrorAt:           status.LastErrorAt,
		LastErrorKind:         status.LastErrorKind,
		SourceUpdatedAt:       status.SourceUpdatedAt,
		UsingLastGood:         status.UsingLastGood,
		LastGoodExpired:       status.LastGoodExpired,
		AgeSeconds:            status.AgeSeconds,
		MaxLastGoodAgeSeconds: status.MaxLastGoodAgeSec,
	}
}

func codexLBDiagnostics(snapshotter *codexlb.Snapshotter) api.OptionalSourceDiagnostics {
	if snapshotter == nil {
		return api.OptionalSourceDiagnostics{Enabled: false, State: "disabled"}
	}
	status := snapshotter.Status(time.Now())
	return api.OptionalSourceDiagnostics{
		Enabled:               status.Enabled,
		Observed:              status.Observed,
		State:                 status.State,
		LastScanAt:            status.LastScanAt,
		LastSuccessAt:         status.LastSuccessAt,
		LastErrorAt:           status.LastErrorAt,
		LastErrorKind:         status.LastErrorKind,
		SourceUpdatedAt:       status.SourceUpdatedAt,
		UsingLastGood:         status.State == "stale",
		LastGoodExpired:       status.State == "expired",
		AgeSeconds:            status.AgeSeconds,
		MaxLastGoodAgeSeconds: status.MaxLastGoodAgeSeconds,
	}
}

func hermesDiagnostics(enabled bool, poller *hermes.Poller) api.OptionalSourceDiagnostics {
	if !enabled || poller == nil {
		return api.OptionalSourceDiagnostics{Enabled: false, State: "disabled"}
	}
	status := poller.Status()
	return api.OptionalSourceDiagnostics{
		Enabled:       true,
		Observed:      status.Observed,
		State:         status.State,
		LastScanAt:    status.LastScanAt,
		LastSuccessAt: status.LastSuccessAt,
		LastErrorAt:   status.LastErrorAt,
		LastErrorKind: status.LastErrorKind,
	}
}

func jsonlDiagnostics(enabled bool, poller *jsonl.Poller, provider wire.Provider) api.OptionalSourceDiagnostics {
	if !enabled || poller == nil {
		return api.OptionalSourceDiagnostics{Enabled: false, State: "disabled"}
	}
	status := poller.Status(provider)
	return api.OptionalSourceDiagnostics{
		Enabled:       true,
		Observed:      status.Observed,
		State:         status.State,
		LastScanAt:    status.LastScanAt,
		LastSuccessAt: status.LastSuccessAt,
		LastErrorAt:   status.LastErrorAt,
		LastErrorKind: status.LastErrorKind,
	}
}

func loadBearerTokens(logger *slog.Logger) (wire.BearerTokens, error) {
	tokens, created, tokenPath, err := wire.LoadOrCreateBearerTokens()
	if err != nil {
		return wire.BearerTokens{}, err
	}
	if created {
		logger.Info("generated bearer token file", "path", tokenPath)
	}
	return tokens, nil
}

func startPeriodicRefresh(ctx context.Context, wg *sync.WaitGroup, st *state.State, hub *sse.Hub, provider wire.Provider, logger *slog.Logger) {
	wg.Add(1)
	go func() {
		defer wg.Done()
		t := time.NewTicker(periodicRefreshInterval)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				refreshCtx, cancel := context.WithTimeout(ctx, 25*time.Second)
				update := st.Refresh(refreshCtx, time.Now())
				cancel()
				if err := hub.PublishSnapshot(update.Snapshot); err != nil {
					logger.Warn("hub publish", "provider", provider, "err", err)
				}
			}
		}
	}()
}

// startJSONLPoller wires the JSONL poller into per-provider
// state ingestion. Each token event bumps the burn rate and broadcasts a
// fresh snapshot through the SSE hub.
func startJSONLPoller(ctx context.Context, wg *sync.WaitGroup, poller *jsonl.Poller, claude, codex *state.State, claudeHub, codexHub *sse.Hub, logger *slog.Logger, claudeActivity accountActivityRecorder, activeClaude activeAccountResolver, enabled map[wire.Provider]bool) {
	emit := makeEventEmitter(claude, codex, claudeHub, codexHub, logger, "jsonl", claudeActivity, activeClaude, enabled)
	poller.SetEmitter(emit)
	wg.Add(1)
	go func() {
		defer wg.Done()
		poller.Run(ctx)
	}()
}

// startHermesPoller wires Hermes' SQLite session deltas into the same per-
// provider state ingestion JSONL uses. Hermes session keys are namespaced
// (`hermes:<id>`) so they stay distinct from JSONL session paths in the
// today_sessions count.
func startHermesPoller(ctx context.Context, wg *sync.WaitGroup, poller *hermes.Poller, claude, codex *state.State, claudeHub, codexHub *sse.Hub, logger *slog.Logger, enabled map[wire.Provider]bool) {
	emit := makeEventEmitter(claude, codex, claudeHub, codexHub, logger, "hermes", nil, nil, enabled)
	poller.SetEmitter(emit)
	wg.Add(1)
	go func() {
		defer wg.Done()
		poller.Run(ctx)
	}()
}

// startAuthFailureGuard reports a typed fatal error after a confirmed run of
// unresolved upstream auth rejections. The caller cancels the root context so
// normal shutdown runs; this goroutine never terminates the process directly.
func startAuthFailureGuard(ctx context.Context, wg *sync.WaitGroup, claude, codex *state.State, enabled map[wire.Provider]bool, logger *slog.Logger, report func(error)) {
	wg.Add(1)
	go func() {
		defer wg.Done()
		t := time.NewTicker(authFailureCheckInterval)
		defer t.Stop()
		if err := runAuthFailureGuard(ctx, t.C, claude, codex, enabled, authFailureThreshold); err != nil {
			logger.Error("self-recovery: unresolved auth rejection threshold reached",
				"err", err,
				"threshold", authFailureThreshold)
			if report != nil {
				report(err)
			}
		}
	}()
}

type authFailureError struct {
	Claude int
	Codex  int
}

type authFailureCounter interface {
	ConsecutiveAuthExpired() int
}

func (e *authFailureError) Error() string {
	return fmt.Sprintf("unresolved auth rejections: claude=%d codex=%d", e.Claude, e.Codex)
}

func runAuthFailureGuard(ctx context.Context, ticks <-chan time.Time, claude, codex authFailureCounter, enabled map[wire.Provider]bool, threshold int) error {
	for {
		select {
		case <-ctx.Done():
			return nil
		case _, open := <-ticks:
			if !open {
				return nil
			}
			cc := 0
			if enabled[wire.ProviderClaude] {
				cc = claude.ConsecutiveAuthExpired()
			}
			cx := 0
			if enabled[wire.ProviderCodex] {
				cx = codex.ConsecutiveAuthExpired()
			}
			if cc >= threshold || cx >= threshold {
				return &authFailureError{Claude: cc, Codex: cx}
			}
		}
	}
}

// startPprofListener exposes net/http/pprof on a localhost-only port for
// heap/goroutine profiling. It is default-off and requires the explicit
// TOKEN_USAGE_ENABLE_PPROF=1 opt-in. Default port 6060; override with
// TOKEN_USAGE_PPROF_PORT.
//
//	go tool pprof http://127.0.0.1:6060/debug/pprof/heap
type pprofRuntime struct {
	server *http.Server
	done   chan error
}

func startPprofListener(logger *slog.Logger) *pprofRuntime {
	if os.Getenv("TOKEN_USAGE_ENABLE_PPROF") != "1" {
		return nil
	}
	port := 6060
	if v := os.Getenv("TOKEN_USAGE_PPROF_PORT"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 && n < 65536 {
			port = n
		}
	}
	addr := fmt.Sprintf("127.0.0.1:%d", port)
	mux := http.NewServeMux()
	mux.HandleFunc("/debug/pprof/", pprof.Index)
	mux.HandleFunc("/debug/pprof/cmdline", pprof.Cmdline)
	mux.HandleFunc("/debug/pprof/profile", pprof.Profile)
	mux.HandleFunc("/debug/pprof/symbol", pprof.Symbol)
	mux.HandleFunc("/debug/pprof/trace", pprof.Trace)
	for _, profile := range []string{"allocs", "block", "goroutine", "heap", "mutex", "threadcreate"} {
		mux.Handle("/debug/pprof/"+profile, pprof.Handler(profile))
	}
	s := &http.Server{
		Addr:              addr,
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
		IdleTimeout:       30 * time.Second,
		MaxHeaderBytes:    16 << 10,
	}
	runtime := &pprofRuntime{server: s, done: make(chan error, 1)}
	go func() {
		logger.Info("pprof listener", "addr", addr)
		err := s.ListenAndServe()
		if err != nil && !errors.Is(err, http.ErrServerClosed) {
			logger.Warn("pprof listener", "err", err)
		}
		runtime.done <- err
	}()
	return runtime
}

func (r *pprofRuntime) shutdown(ctx context.Context) error {
	if r == nil {
		return nil
	}
	err := r.server.Shutdown(ctx)
	if errors.Is(err, http.ErrServerClosed) {
		err = nil
	}
	select {
	case <-r.done:
		return err
	case <-ctx.Done():
		return errors.Join(err, ctx.Err())
	}
}

// shutdownRuntime enforces the lifecycle order in one testable, clock-free
// unit: root cancellation, stream admission/closure, worker wait, then HTTP
// listener shutdown (main + optional profiler).
func shutdownRuntime(cancelRoot, closeStreams, waitWorkers func(), shutdownHTTP func() error) error {
	cancelRoot()
	closeStreams()
	waitWorkers()
	return shutdownHTTP()
}

type accountActivityRecorder interface {
	Ingest(jsonl.TokenEvent, time.Time)
}

type activeAccountResolver interface {
	ActiveAccountNumber() int
}

// makeEventEmitter returns a closure that ingests an event into the right
// provider's state and publishes the resulting snapshot through its hub.
func makeEventEmitter(claude, codex *state.State, claudeHub, codexHub *sse.Hub, logger *slog.Logger, source string, claudeActivity accountActivityRecorder, activeClaude activeAccountResolver, enabled map[wire.Provider]bool) func(jsonl.TokenEvent) {
	return func(ev jsonl.TokenEvent) {
		if !enabled[ev.Provider] {
			return
		}
		now := time.Now()
		ev.Source = source
		var snap wire.UsageSnapshot
		var hub *sse.Hub
		switch ev.Provider {
		case wire.ProviderClaude:
			if claudeActivity != nil {
				if ev.AccountNumber <= 0 && activeClaude != nil {
					ev.AccountNumber = activeClaude.ActiveAccountNumber()
				}
				claudeActivity.Ingest(ev, now)
			}
			snap = claude.IngestEvent(ev, now)
			hub = claudeHub
		case wire.ProviderCodex:
			snap = codex.IngestEvent(ev, now)
			hub = codexHub
		default:
			return
		}
		if err := hub.PublishSnapshot(snap); err != nil {
			logger.Warn("hub publish ("+source+")", "provider", ev.Provider, "err", err)
		}
	}
}

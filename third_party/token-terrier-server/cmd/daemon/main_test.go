package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/token-terrier/server-go/internal/auth"
	"github.com/codemoo/token-terrier/server-go/internal/jsonl"
	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestLoadBearerTokensLogsPathWithoutTokenValues(t *testing.T) {
	unsetTestEnv(t, "TOKEN_USAGE_CLAUDE_TOKEN")
	unsetTestEnv(t, "TOKEN_USAGE_CODEX_TOKEN")
	home := t.TempDir()
	t.Setenv("HOME", home)

	var logs bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&logs, nil))
	tokens, err := loadBearerTokens(logger)
	if err != nil {
		t.Fatalf("loadBearerTokens() error = %v", err)
	}

	output := logs.String()
	if strings.Contains(output, tokens.Claude) || strings.Contains(output, tokens.Codex) {
		t.Fatalf("startup log contains a bearer token: %q", output)
	}
	wantPath := filepath.Join(home, ".config", "token-usage", "tokens.json")
	if !strings.Contains(output, wantPath) {
		t.Fatalf("startup log = %q, want generated path %q", output, wantPath)
	}
}

func unsetTestEnv(t *testing.T, key string) {
	t.Helper()
	old, present := os.LookupEnv(key)
	if err := os.Unsetenv(key); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if present {
			_ = os.Setenv(key, old)
		} else {
			_ = os.Unsetenv(key)
		}
	})
}

func TestReadEmbeddedConfigurationKeepsLeaseOpenAndAvoidsTokenFile(t *testing.T) {
	claude := strings.Repeat("a", 64)
	codex := strings.Repeat("b", 64)
	input := strings.NewReader(`{"schema":1,"claude_token":"` + claude + `","codex_token":"` + codex + `","codex_lb_api_key":"lb-key","codex_lb_base_url":"http://127.0.0.1:2455"}` + "\nlease")
	configuration, lease, err := readEmbeddedConfiguration(input)
	if err != nil {
		t.Fatalf("readEmbeddedConfiguration() error = %v", err)
	}
	if !configuration.embedded || configuration.tokens.Claude != claude || configuration.tokens.Codex != codex {
		t.Fatalf("configuration = %+v", configuration)
	}
	if configuration.codexLBAPIKey != "lb-key" || configuration.codexLBBaseURL != "http://127.0.0.1:2455" {
		t.Fatalf("codex-lb configuration = %+v", configuration)
	}
	remaining, err := io.ReadAll(lease)
	if err != nil || string(remaining) != "lease" {
		t.Fatalf("lease remainder = %q, %v", remaining, err)
	}
}

func TestReadEmbeddedConfigurationRejectsUnsafeInput(t *testing.T) {
	tests := []string{
		`{"schema":2,"claude_token":"` + strings.Repeat("a", 64) + `","codex_token":"` + strings.Repeat("b", 64) + `"}` + "\n",
		`{"schema":1,"claude_token":"short","codex_token":"` + strings.Repeat("b", 64) + `"}` + "\n",
		`{"schema":1,"claude_token":"` + strings.Repeat("a", 64) + `","codex_token":"` + strings.Repeat("a", 64) + `"}` + "\n",
		`{"schema":1,"claude_token":"` + strings.Repeat("a", 64) + `","codex_token":"` + strings.Repeat("b", 64) + `","unknown":true}` + "\n",
	}
	for _, input := range tests {
		if _, _, err := readEmbeddedConfiguration(strings.NewReader(input)); err == nil {
			t.Fatalf("readEmbeddedConfiguration(%q) error = nil", input)
		}
	}
}

func TestEmbeddedRuntimeDisablesCredentialRefreshAndWrites(t *testing.T) {
	local := &auth.LocalSource{
		ClaudePath: filepath.Join(t.TempDir(), ".credentials.json"),
		CodexPath:  filepath.Join(t.TempDir(), "auth.json"),
	}
	source := &embeddedCredentialSource{source: local}
	if err := source.Write(context.Background(), wire.ProviderClaude, []byte(`{}`)); !errors.Is(err, errEmbeddedCredentialWriteDisabled) {
		t.Fatalf("embedded credential write error = %v", err)
	}
	if _, err := os.Stat(local.ClaudePath); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("embedded credential write created a file: %v", err)
	}
	store := auth.NewCredentialStore(source)
	if refresher := configuredRefresher(runtimeConfiguration{embedded: true}, store); refresher != nil {
		t.Fatalf("embedded runtime configured an OAuth refresher: %T", refresher)
	}
	if refresher := configuredRefresher(runtimeConfiguration{}, store); refresher == nil {
		t.Fatal("standalone runtime lost its OAuth refresher")
	}
}

func TestEmbeddedLeaseCancelsOnEOF(t *testing.T) {
	reader, writer := io.Pipe()
	ctx, cancel := embeddedLeaseContext(context.Background(), reader)
	defer cancel()
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-ctx.Done():
	case <-time.After(time.Second):
		t.Fatal("lease EOF did not cancel context")
	}
}

func TestEmbeddedRuntimeBindsEphemeralLoopbackAndRejectsWrongBearer(t *testing.T) {
	for _, key := range []string{
		"TOKEN_USAGE_DISABLE_CLAUDE",
		"TOKEN_USAGE_DISABLE_CODEX",
		"TOKEN_USAGE_DISABLE_JSONL",
		"TOKEN_USAGE_DISABLE_HERMES",
		"TOKEN_USAGE_DISABLE_CLAUDE_SWAP",
		"TOKEN_USAGE_DISABLE_CODEX_ACCOUNTS",
	} {
		t.Setenv(key, "1")
	}
	reader, writer := io.Pipe()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- runConfigured(ctx, slog.New(slog.NewTextHandler(io.Discard, nil)), runtimeConfiguration{
			embedded: true,
			tokens: wire.BearerTokens{
				Claude: strings.Repeat("a", 64),
				Codex:  strings.Repeat("b", 64),
			},
			bootstrap: writer,
		})
	}()

	var bootstrap embeddedBootstrap
	if err := json.NewDecoder(reader).Decode(&bootstrap); err != nil {
		cancel()
		t.Fatalf("decode bootstrap: %v", err)
	}
	if bootstrap.Schema != 1 || bootstrap.PID != os.Getpid() || bootstrap.Host != "127.0.0.1" || bootstrap.Port < 1 {
		cancel()
		t.Fatalf("bootstrap = %+v", bootstrap)
	}
	request, err := http.NewRequest(http.MethodGet, fmt.Sprintf("http://127.0.0.1:%d/claude/sse", bootstrap.Port), nil)
	if err != nil {
		cancel()
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer "+strings.Repeat("x", 64))
	response, err := (&http.Client{Timeout: time.Second}).Do(request)
	if err != nil {
		cancel()
		t.Fatal(err)
	}
	_ = response.Body.Close()
	if response.StatusCode != http.StatusUnauthorized {
		cancel()
		t.Fatalf("wrong bearer status = %d, want 401", response.StatusCode)
	}

	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("runConfigured() error = %v", err)
		}
	case <-time.After(8 * time.Second):
		t.Fatal("embedded runtime did not shut down")
	}
}

func TestJSONLDiagnosticsReportsDisabledAndUnobserved(t *testing.T) {
	disabled := jsonlDiagnostics(false, nil, wire.ProviderClaude)
	if disabled.Enabled || disabled.State != "disabled" {
		t.Fatalf("disabled diagnostics = %+v", disabled)
	}

	poller := jsonl.NewPoller(nil, nil)
	enabled := jsonlDiagnostics(true, poller, wire.ProviderCodex)
	if !enabled.Enabled || enabled.Observed || enabled.State != "unobserved" {
		t.Fatalf("enabled diagnostics = %+v", enabled)
	}
}

type authCounterStub int

func (c authCounterStub) ConsecutiveAuthExpired() int { return int(c) }

func TestAuthFailureGuardUsesInjectedTicksAndEnablement(t *testing.T) {
	enabled := map[wire.Provider]bool{
		wire.ProviderClaude: true,
		wire.ProviderCodex:  true,
	}
	ticks := make(chan time.Time, 1)
	ticks <- time.Time{}
	err := runAuthFailureGuard(context.Background(), ticks, authCounterStub(authFailureThreshold), authCounterStub(0), enabled, authFailureThreshold)
	var authErr *authFailureError
	if !errors.As(err, &authErr) || authErr.Claude != authFailureThreshold {
		t.Fatalf("guard error = %v, want authFailureError", err)
	}

	// A disabled provider cannot terminate an otherwise healthy server even
	// if stale in-memory state happens to retain a high historical count.
	disabledTicks := make(chan time.Time, 1)
	disabledTicks <- time.Time{}
	close(disabledTicks)
	err = runAuthFailureGuard(
		context.Background(),
		disabledTicks,
		authCounterStub(authFailureThreshold),
		authCounterStub(0),
		map[wire.Provider]bool{wire.ProviderClaude: false, wire.ProviderCodex: true},
		authFailureThreshold,
	)
	if err != nil {
		t.Fatalf("disabled provider guard error = %v, want nil", err)
	}
}

func TestShutdownRuntimeOrder(t *testing.T) {
	var order []string
	wantErr := errors.New("shutdown failed")
	gotErr := shutdownRuntime(
		func() { order = append(order, "root_cancel") },
		func() { order = append(order, "close_streams") },
		func() { order = append(order, "wait_workers") },
		func() error {
			order = append(order, "http_shutdown")
			return wantErr
		},
	)
	if got := strings.Join(order, ","); got != "root_cancel,close_streams,wait_workers,http_shutdown" {
		t.Fatalf("shutdown order = %q", got)
	}
	if !errors.Is(gotErr, wantErr) {
		t.Fatalf("shutdown error = %v, want %v", gotErr, wantErr)
	}
}

func TestCurrentBuildInfoUsesLdflagsValues(t *testing.T) {
	oldVersion, oldSHA := serverVersion, serverGitSHA
	oldDirty, oldTime := serverGitDirty, serverBuildTime
	t.Cleanup(func() {
		serverVersion, serverGitSHA = oldVersion, oldSHA
		serverGitDirty, serverBuildTime = oldDirty, oldTime
	})
	serverVersion = "1.2.3"
	serverGitSHA = "0123456789abcdef"
	serverGitDirty = "false"
	serverBuildTime = "2026-07-16T04:05:06Z"

	got := currentBuildInfo()
	if got.Version != serverVersion || got.GitSHA != serverGitSHA {
		t.Fatalf("version provenance = %+v", got)
	}
	if got.Dirty == nil || *got.Dirty || got.BuildTime == nil || *got.BuildTime != serverBuildTime {
		t.Fatalf("dirty/build time = %v/%v", got.Dirty, got.BuildTime)
	}
	if got.Schema != 1 || len(got.Capabilities) == 0 {
		t.Fatalf("schema/capabilities = %d/%v", got.Schema, got.Capabilities)
	}
}

func TestPprofIsDefaultOff(t *testing.T) {
	t.Setenv("TOKEN_USAGE_ENABLE_PPROF", "0")
	if got := startPprofListener(slog.Default()); got != nil {
		t.Fatal("pprof started without TOKEN_USAGE_ENABLE_PPROF=1")
	}
}

func TestCancellationRunErrorTreatsParentSignalAsCleanShutdown(t *testing.T) {
	parent, cancelParent := context.WithCancel(context.Background())
	root, cancelRoot := context.WithCancelCause(parent)
	cancelParent()
	<-root.Done()
	if err := cancellationRunError(parent, root); err != nil {
		t.Fatalf("parent cancellation returned error: %v", err)
	}
	cancelRoot(nil)

	parent = context.Background()
	root, cancelRoot = context.WithCancelCause(parent)
	want := errors.New("confirmed auth failure")
	cancelRoot(want)
	if got := cancellationRunError(parent, root); !errors.Is(got, want) {
		t.Fatalf("child cancellation error = %v, want %v", got, want)
	}
}

func TestValidateHTTPBindRequiresOptInOutsideLoopback(t *testing.T) {
	for _, host := range []string{"127.0.0.1", "127.0.0.2", "::1", "[::1]", "localhost."} {
		if err := validateHTTPBind(host, false); err != nil {
			t.Errorf("loopback %q rejected: %v", host, err)
		}
	}
	for _, host := range []string{"0.0.0.0", "::", "192.0.2.10", "server.example"} {
		if err := validateHTTPBind(host, false); err == nil {
			t.Errorf("non-loopback %q accepted without opt-in", host)
		}
		if err := validateHTTPBind(host, true); err != nil {
			t.Errorf("explicit opt-in for %q rejected: %v", host, err)
		}
	}
}

func TestManagedParentContextValidatesDirectParent(t *testing.T) {
	t.Setenv("HMUX_USAGE_PARENT_PID", "42")
	ctx, stop, err := managedParentContext(context.Background(), func() int { return 42 })
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-ctx.Done():
		t.Fatal("matching parent context was cancelled")
	default:
	}
	stop()
	<-ctx.Done()

	if _, _, err := managedParentContext(context.Background(), func() int { return 41 }); err == nil {
		t.Fatal("mismatched managed parent was accepted")
	}
	t.Setenv("HMUX_USAGE_PARENT_PID", "not-a-pid")
	if _, _, err := managedParentContext(context.Background(), func() int { return 42 }); err == nil {
		t.Fatal("malformed managed parent was accepted")
	}
}

func TestManagedParentContextPreservesStandaloneMode(t *testing.T) {
	t.Setenv("HMUX_USAGE_PARENT_PID", "")
	ctx, stop, err := managedParentContext(context.Background(), func() int { return 1 })
	if err != nil {
		t.Fatal(err)
	}
	stop()
	<-ctx.Done()
}

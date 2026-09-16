package wire

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"testing"
)

const (
	testClaudeToken = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	testCodexToken  = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
)

func TestIsAuthorized(t *testing.T) {
	tests := []struct {
		name     string
		header   string
		expected string
		want     bool
	}{
		{name: "matching token", header: "Bearer " + testClaudeToken, expected: testClaudeToken, want: true},
		{name: "missing header", expected: testClaudeToken},
		{name: "wrong scheme", header: "Basic " + testClaudeToken, expected: testClaudeToken},
		{name: "wrong token", header: "Bearer " + testCodexToken, expected: testClaudeToken},
		{name: "empty expected and empty given", header: "Bearer ", expected: ""},
		{name: "whitespace expected", header: "Bearer    ", expected: "   "},
		{name: "weak expected", header: "Bearer short", expected: "short"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := IsAuthorized(tt.header, tt.expected); got != tt.want {
				t.Fatalf("IsAuthorized() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestLoadOrCreateBearerTokensRejectsInvalidOverrides(t *testing.T) {
	tests := []struct {
		name  string
		value string
	}{
		{name: "empty", value: ""},
		{name: "whitespace only", value: " \t\n"},
		{name: "too short", value: "s3cr3t"},
		{name: "contains whitespace", value: strings.Repeat("a", minimumBearerTokenLength-1) + " "},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			home := t.TempDir()
			t.Setenv("HOME", home)
			t.Setenv("TOKEN_USAGE_CLAUDE_TOKEN", tt.value)
			t.Setenv("TOKEN_USAGE_CODEX_TOKEN", testCodexToken)

			_, _, _, err := LoadOrCreateBearerTokens()
			if err == nil {
				t.Fatal("LoadOrCreateBearerTokens() error = nil, want invalid override error")
			}
			if tt.value != "" && strings.Contains(err.Error(), tt.value) {
				t.Fatalf("error leaked token value: %v", err)
			}
			if _, statErr := os.Stat(bearerTokenPath(home)); !errors.Is(statErr, os.ErrNotExist) {
				t.Fatalf("invalid overrides created token file: %v", statErr)
			}
		})
	}
}

func TestLoadOrCreateBearerTokensRejectsEqualOverrides(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("TOKEN_USAGE_CLAUDE_TOKEN", testClaudeToken)
	t.Setenv("TOKEN_USAGE_CODEX_TOKEN", testClaudeToken)

	_, _, _, err := LoadOrCreateBearerTokens()
	if err == nil {
		t.Fatal("LoadOrCreateBearerTokens() error = nil, want duplicate-token error")
	}
}

func TestLoadOrCreateBearerTokensWithOverridesDoesNotCreateFile(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("TOKEN_USAGE_CLAUDE_TOKEN", testClaudeToken)
	t.Setenv("TOKEN_USAGE_CODEX_TOKEN", testCodexToken)

	tokens, created, path, err := LoadOrCreateBearerTokens()
	if err != nil {
		t.Fatalf("LoadOrCreateBearerTokens() error = %v", err)
	}
	if tokens != (BearerTokens{Claude: testClaudeToken, Codex: testCodexToken}) {
		t.Fatalf("tokens = %#v, want environment overrides", tokens)
	}
	if created {
		t.Fatal("created = true, want false")
	}
	if path != "" {
		t.Fatalf("path = %q, want empty when no file is used", path)
	}
	if _, statErr := os.Stat(bearerTokenPath(home)); !errors.Is(statErr, os.ErrNotExist) {
		t.Fatalf("environment-only configuration created token file: %v", statErr)
	}
}

func TestLoadOrCreateBearerTokensRejectsInvalidFiles(t *testing.T) {
	tests := []struct {
		name string
		body string
	}{
		{name: "empty object", body: `{}`},
		{name: "missing codex", body: `{"claude":"` + testClaudeToken + `"}`},
		{name: "whitespace", body: `{"claude":"` + testClaudeToken + `","codex":"   "}`},
		{name: "short", body: `{"claude":"` + testClaudeToken + `","codex":"short"}`},
		{name: "same token", body: `{"claude":"` + testClaudeToken + `","codex":"` + testClaudeToken + `"}`},
		{name: "invalid json", body: `{"claude":`},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			home := t.TempDir()
			unsetEnv(t, "TOKEN_USAGE_CLAUDE_TOKEN")
			unsetEnv(t, "TOKEN_USAGE_CODEX_TOKEN")
			t.Setenv("HOME", home)
			path := bearerTokenPath(home)
			if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(path, []byte(tt.body), 0o600); err != nil {
				t.Fatal(err)
			}

			_, _, _, err := LoadOrCreateBearerTokens()
			if err == nil {
				t.Fatal("LoadOrCreateBearerTokens() error = nil, want invalid file error")
			}
		})
	}
}

func TestLoadOrCreateBearerTokensRejectsOversizedFile(t *testing.T) {
	home := t.TempDir()
	unsetEnv(t, "TOKEN_USAGE_CLAUDE_TOKEN")
	unsetEnv(t, "TOKEN_USAGE_CODEX_TOKEN")
	t.Setenv("HOME", home)
	path := bearerTokenPath(home)
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, bytes.Repeat([]byte("x"), maxBearerTokenFileSize+1), 0o600); err != nil {
		t.Fatal(err)
	}

	_, _, _, err := LoadOrCreateBearerTokens()
	if err == nil {
		t.Fatal("LoadOrCreateBearerTokens() error = nil, want oversized-file error")
	}
	if !strings.Contains(err.Error(), "exceeds maximum size") {
		t.Fatalf("LoadOrCreateBearerTokens() error = %v, want maximum-size error", err)
	}
}

func TestLoadOrCreateBearerTokensValidatesFileBeforeApplyingPartialOverride(t *testing.T) {
	home := t.TempDir()
	unsetEnv(t, "TOKEN_USAGE_CODEX_TOKEN")
	t.Setenv("HOME", home)
	t.Setenv("TOKEN_USAGE_CLAUDE_TOKEN", testClaudeToken)
	writeBearerTokenFile(t, bearerTokenPath(home), BearerTokens{Codex: testCodexToken}, 0o600)

	_, _, _, err := LoadOrCreateBearerTokens()
	if err == nil {
		t.Fatal("LoadOrCreateBearerTokens() error = nil, want invalid fallback file error")
	}
}

func TestLoadOrCreateBearerTokensRejectsDuplicateEffectiveTokens(t *testing.T) {
	home := t.TempDir()
	unsetEnv(t, "TOKEN_USAGE_CODEX_TOKEN")
	t.Setenv("HOME", home)
	t.Setenv("TOKEN_USAGE_CLAUDE_TOKEN", testCodexToken)
	writeBearerTokenFile(t, bearerTokenPath(home), BearerTokens{
		Claude: testClaudeToken,
		Codex:  testCodexToken,
	}, 0o600)

	_, _, _, err := LoadOrCreateBearerTokens()
	if err == nil {
		t.Fatal("LoadOrCreateBearerTokens() error = nil, want duplicate effective-token error")
	}
}

func TestLoadOrCreateBearerTokensRepairsInsecureFilePermissions(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Windows ACLs are not represented by Unix permission bits")
	}
	home := t.TempDir()
	unsetEnv(t, "TOKEN_USAGE_CLAUDE_TOKEN")
	unsetEnv(t, "TOKEN_USAGE_CODEX_TOKEN")
	t.Setenv("HOME", home)
	path := bearerTokenPath(home)
	want := BearerTokens{Claude: testClaudeToken, Codex: testCodexToken}
	writeBearerTokenFile(t, path, want, 0o644)

	got, created, _, err := LoadOrCreateBearerTokens()
	if err != nil {
		t.Fatalf("LoadOrCreateBearerTokens() error = %v", err)
	}
	if got != want {
		t.Fatalf("tokens = %#v, want %#v", got, want)
	}
	if created {
		t.Fatal("created = true, want false")
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if gotMode := info.Mode().Perm(); gotMode != 0o600 {
		t.Fatalf("mode = %#o, want 0600", gotMode)
	}
}

func TestLoadOrCreateBearerTokensConcurrentFirstStart(t *testing.T) {
	home := t.TempDir()
	unsetEnv(t, "TOKEN_USAGE_CLAUDE_TOKEN")
	unsetEnv(t, "TOKEN_USAGE_CODEX_TOKEN")
	t.Setenv("HOME", home)

	const goroutines = 32
	start := make(chan struct{})
	results := make(chan struct {
		tokens  BearerTokens
		created bool
		path    string
		err     error
	}, goroutines)
	var wg sync.WaitGroup
	for range goroutines {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			tokens, created, path, err := LoadOrCreateBearerTokens()
			results <- struct {
				tokens  BearerTokens
				created bool
				path    string
				err     error
			}{tokens: tokens, created: created, path: path, err: err}
		}()
	}
	close(start)
	wg.Wait()
	close(results)

	wantPath := bearerTokenPath(home)
	var wantTokens BearerTokens
	createdCount := 0
	for result := range results {
		if result.err != nil {
			t.Fatalf("LoadOrCreateBearerTokens() error = %v", result.err)
		}
		if result.path != wantPath {
			t.Errorf("path = %q, want %q", result.path, wantPath)
		}
		if result.created {
			createdCount++
		}
		if wantTokens == (BearerTokens{}) {
			wantTokens = result.tokens
		} else if result.tokens != wantTokens {
			t.Errorf("concurrent caller got %#v, want %#v", result.tokens, wantTokens)
		}
	}
	if createdCount != 1 {
		t.Fatalf("created count = %d, want 1", createdCount)
	}
	if err := validateBearerTokens(wantTokens, "generated token file"); err != nil {
		t.Fatalf("generated tokens are invalid: %v", err)
	}
	entries, err := os.ReadDir(filepath.Dir(wantPath))
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Name() != filepath.Base(wantPath) {
		t.Fatalf("token directory entries = %v, want only %q", entryNames(entries), filepath.Base(wantPath))
	}
}

func writeBearerTokenFile(t *testing.T, path string, tokens BearerTokens, mode os.FileMode) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	data, err := json.Marshal(tokens)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, mode); err != nil {
		t.Fatal(err)
	}
	// os.WriteFile preserves an existing file's mode, so set it explicitly.
	if err := os.Chmod(path, mode); err != nil {
		t.Fatal(err)
	}
}

func bearerTokenPath(home string) string {
	return filepath.Join(home, ".config", "token-usage", "tokens.json")
}

func unsetEnv(t *testing.T, key string) {
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

func entryNames(entries []os.DirEntry) []string {
	names := make([]string, len(entries))
	for i, entry := range entries {
		names[i] = entry.Name()
	}
	return names
}

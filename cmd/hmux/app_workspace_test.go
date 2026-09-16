package main

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
)

const validAppWorkspaceSSHFixture = `host hmux-home
user owner
hostname home.internal
port 22
batchmode yes
forwardagent no
clearallforwardings yes
proxyjump dmz
identityfile ~/.ssh/id_ed25519
`

func TestAppWorkspaceHomeSourceIsDeterministicAndScoped(t *testing.T) {
	cfg := config.DefaultClientConfig()
	cfg.Role = "home"
	cfg.StateDir = filepath.Join(t.TempDir(), "state-a")
	first, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	second, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if first != second || !validAppWorkspaceSourceKey(first) {
		t.Fatalf("unstable or malformed source keys: %q %q", first, second)
	}
	cfg.StateDir = filepath.Join(filepath.Dir(cfg.StateDir), "state-b")
	changed, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if changed == first {
		t.Fatal("state directory change did not change the workspace source")
	}
	cfg.ClientID = "another-client"
	changedAgain, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if changedAgain == changed {
		t.Fatal("client identity change did not change the workspace source")
	}
}

func TestAppWorkspaceRemoteSourceUsesOnlyEffectiveSSHProbe(t *testing.T) {
	dir := t.TempDir()
	logPath := filepath.Join(dir, "ssh.args")
	writeAppWorkspaceFakeSSH(t, dir, `#!/bin/sh
set -eu
printf '%s\n' "$@" >"$HMUX_TEST_SSH_ARGS"
printf '%s' "$HMUX_TEST_SSH_OUTPUT"
`)
	t.Setenv("HMUX_TEST_SSH_ARGS", logPath)
	t.Setenv("HMUX_TEST_SSH_OUTPUT", validAppWorkspaceSSHFixture)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	first, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	second, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if first != second || !validAppWorkspaceSourceKey(first) {
		t.Fatalf("unstable or malformed remote source keys: %q %q", first, second)
	}
	arguments, err := os.ReadFile(logPath)
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(strings.TrimSpace(string(arguments)), "\n")
	want := []string{
		"-G", "-o", "BatchMode=yes", "-o", "ForwardAgent=no",
		"-o", "ClearAllForwardings=yes", cfg.HomeAlias,
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("ssh arguments=%v want=%v", got, want)
	}
	for _, argument := range got {
		if argument == cfg.AgentPath || argument == "--" {
			t.Fatalf("workspace source probe attempted a remote command: %v", got)
		}
	}

	t.Setenv("HMUX_TEST_SSH_OUTPUT", strings.Replace(validAppWorkspaceSSHFixture, "home.internal", "replacement.internal", 1))
	changed, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if changed == first {
		t.Fatal("effective SSH routing change did not change the workspace source")
	}
	cfg.AgentPath = "/usr/local/bin/hmux-agent"
	changedAgain, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if changedAgain == changed {
		t.Fatal("agent path change did not change the workspace source")
	}
}

func TestAppWorkspaceRemoteSourceRejectsMalformedAndOversizedOutput(t *testing.T) {
	dir := t.TempDir()
	writeAppWorkspaceFakeSSH(t, dir, "#!/bin/sh\nprintf '%s' \"$HMUX_TEST_SSH_OUTPUT\"\n")
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	for name, output := range map[string]string{
		"empty":              "",
		"missing routing":    "host hmux-home\nbatchmode yes\nforwardagent no\nclearallforwardings yes\n",
		"malformed line":     validAppWorkspaceSSHFixture + "broken\n",
		"bad port":           strings.Replace(validAppWorkspaceSSHFixture, "port 22", "port 70000", 1),
		"forwarding enabled": strings.Replace(validAppWorkspaceSSHFixture, "forwardagent no", "forwardagent yes", 1),
	} {
		t.Run(name, func(t *testing.T) {
			t.Setenv("HMUX_TEST_SSH_OUTPUT", output)
			if _, err := appWorkspaceSourceKey(t.Context(), cfg); !errors.Is(err, errAppWorkspaceSource) {
				t.Fatalf("error=%v", err)
			}
		})
	}
	if err := validateAppWorkspaceSSHOutput([]byte(validAppWorkspaceSSHFixture + "other x\x00y\n")); !errors.Is(err, errAppWorkspaceSource) {
		t.Fatalf("embedded NUL error=%v", err)
	}
	t.Setenv("HMUX_TEST_SSH_OUTPUT", validAppWorkspaceSSHFixture+"other "+strings.Repeat("x", appWorkspaceSSHOutputLimit))
	if _, err := appWorkspaceSourceKey(t.Context(), cfg); !errors.Is(err, errAppWorkspaceSource) {
		t.Fatalf("oversized output error=%v", err)
	}
}

func TestAppWorkspaceSourceRejectsInvalidRoleAndAlias(t *testing.T) {
	cfg := config.DefaultClientConfig()
	cfg.Role = "other"
	if _, err := appWorkspaceSourceKey(t.Context(), cfg); !errors.Is(err, errAppWorkspaceSource) {
		t.Fatalf("invalid role error=%v", err)
	}
	cfg.Role = "remote"
	for _, alias := range []string{"", "-option", "bad alias", "bad;alias"} {
		cfg.HomeAlias = alias
		if _, err := appWorkspaceSourceKey(t.Context(), cfg); !errors.Is(err, errAppWorkspaceSource) {
			t.Fatalf("alias %q error=%v", alias, err)
		}
	}
}

func TestAppWorkspaceSourceHonorsCancellationAndTimeout(t *testing.T) {
	dir := t.TempDir()
	writeAppWorkspaceFakeSSH(t, dir, "#!/bin/sh\nexec sleep 30\n")
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	started := time.Now()
	if _, err := appWorkspaceSourceKey(ctx, cfg); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancellation error=%v", err)
	}
	if time.Since(started) > time.Second {
		t.Fatalf("canceled source probe took too long: %s", time.Since(started))
	}

	started = time.Now()
	if _, err := appWorkspaceSourceKey(t.Context(), cfg); !errors.Is(err, errAppWorkspaceSource) {
		t.Fatalf("timeout error=%v", err)
	}
	if elapsed := time.Since(started); elapsed < 2*time.Second || elapsed > 6*time.Second {
		t.Fatalf("source probe timeout was not bounded: %s", elapsed)
	}
}

func TestRequireAppWorkspaceSourceRequiresValidMatchingBinding(t *testing.T) {
	cfg := config.DefaultClientConfig()
	cfg.Role = "home"
	cfg.StateDir = t.TempDir()
	key, err := appWorkspaceSourceKey(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if err := requireAppWorkspaceSource(t.Context(), cfg, nil); err == nil {
		t.Fatal("unavailable source environment was accepted")
	}
	if err := requireAppWorkspaceSource(t.Context(), cfg, func(string) string { return "" }); err == nil ||
		!strings.Contains(err.Error(), "source key is required") {
		t.Fatalf("missing source key error=%v", err)
	}
	if err := requireAppWorkspaceSource(t.Context(), cfg, func(string) string { return key }); err != nil {
		t.Fatalf("matching source key was rejected: %v", err)
	}
	for _, invalid := range []string{"short", strings.ToUpper(key), key[:63] + "g"} {
		if err := requireAppWorkspaceSource(t.Context(), cfg, func(string) string { return invalid }); err == nil ||
			!strings.Contains(err.Error(), "source key is invalid") {
			t.Fatalf("invalid key %q error=%v", invalid, err)
		}
	}
	cfg.StateDir = filepath.Join(cfg.StateDir, "changed")
	if err := requireAppWorkspaceSource(t.Context(), cfg, func(string) string { return key }); err == nil ||
		!strings.Contains(err.Error(), "configuration changed; reopen HMux") {
		t.Fatalf("mismatch error=%v", err)
	}
}

func writeAppWorkspaceFakeSSH(t *testing.T, directory, script string) {
	t.Helper()
	path := filepath.Join(directory, "ssh")
	if err := os.WriteFile(path, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
}

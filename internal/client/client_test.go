package client

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/archive/terminal/frame"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

func TestResolveSession(t *testing.T) {
	value := model.Catalog{Sessions: []model.Session{{ID: "$1", Name: "코덱 main", Alias: "main"}, {ID: "$2", Name: "api"}}}
	for input, want := range map[string]string{"$1": "$1", "코덱 main": "$1", "main": "$1", "api": "$2"} {
		got, err := ResolveSession(value, input)
		if err != nil || got != want {
			t.Fatalf("%q got=%q err=%v", input, got, err)
		}
	}
	if _, err := ResolveSession(value, "missing; touch /tmp/x"); err == nil {
		t.Fatal("unsafe/missing input resolved")
	}
}

func TestSafeRemotePathUsesAStrictExecutableAllowlist(t *testing.T) {
	for _, value := range []string{"~/.local/bin/hmux-agent", "/usr/local/bin/hmux-agent"} {
		if !safeRemotePath(value) {
			t.Errorf("safe remote executable rejected: %q", value)
		}
	}
	for _, value := range []string{
		"", "hmux-agent", "../hmux-agent", "/tmp/../hmux-agent",
		"~/.local/bin/hmux'agent", "~/.local/bin/hmux\\agent",
		"~/.local/bin/hmux agent", "~/.local/bin/hmux;agent",
	} {
		if safeRemotePath(value) {
			t.Errorf("unsafe remote executable accepted: %q", value)
		}
	}
}

func TestRemoteAliasAndTerminationUseFixedSSHArgumentArrays(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncase \"$*\" in *capabilities) printf 'expected-identity-v1\\nsession-visibility-v1\\n'; exit 0;; esac\ncat > \"$HMUX_FAKE_STDIN\"\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	argsPath := filepath.Join(dir, "args")
	stdinPath := filepath.Join(dir, "stdin")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_STDIN", stdinPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	if err := SetAlias(context.Background(), cfg, "$7", "friendly alias"); err != nil {
		t.Fatal(err)
	}
	argsData, _ := os.ReadFile(argsPath)
	gotArgs := strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "alias-set", `\$7`)
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("alias ssh args=%v", gotArgs)
	}
	stdinData, _ := os.ReadFile(stdinPath)
	if string(stdinData) != "friendly alias\n" {
		t.Fatalf("alias stdin=%q", stdinData)
	}
	if err := SetAliasExpected(context.Background(), cfg, "$7", 1700000000, "expected alias"); err != nil {
		t.Fatal(err)
	}
	argsData, _ = os.ReadFile(argsPath)
	gotArgs = strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs = append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "alias-set", "--created-at", "1700000000", `\$7`)
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("expected alias ssh args=%v", gotArgs)
	}
	if err := SetHiddenExpected(context.Background(), cfg, "$7", 1700000000, true); err != nil {
		t.Fatal(err)
	}
	argsData, _ = os.ReadFile(argsPath)
	gotArgs = strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs = append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "hidden-set", "--created-at", "1700000000", `\$7`)
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("hidden ssh args=%v", gotArgs)
	}
	stdinData, _ = os.ReadFile(stdinPath)
	if string(stdinData) != "true\n" {
		t.Fatalf("hidden stdin=%q", stdinData)
	}

	if err := TerminateSession(context.Background(), cfg, "$7"); err != nil {
		t.Fatal(err)
	}
	argsData, _ = os.ReadFile(argsPath)
	gotArgs = strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs = append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "terminate", "--confirmed", `\$7`)
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("termination ssh args=%v", gotArgs)
	}
	if err := TerminateSessionExpected(context.Background(), cfg, "$7", 1700000000); err != nil {
		t.Fatal(err)
	}
	argsData, _ = os.ReadFile(argsPath)
	gotArgs = strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs = append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "terminate", "--confirmed", "--created-at", "1700000000", `\$7`)
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("expected termination ssh args=%v", gotArgs)
	}
}

func TestExpectedRemoteOperationsFailClosedWithoutIdentityCapability(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\nexit 1\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	for name, run := range map[string]func() error{
		"attach": func() error { return AttachExpected(cfg, "$7", 1700000000, true) },
		"alias": func() error {
			return SetAliasExpected(context.Background(), cfg, "$7", 1700000000, "expected alias")
		},
		"hidden": func() error {
			return SetHiddenExpected(context.Background(), cfg, "$7", 1700000000, true)
		},
		"terminate": func() error {
			return TerminateSessionExpected(context.Background(), cfg, "$7", 1700000000)
		},
	} {
		t.Run(name, func(t *testing.T) {
			if err := run(); err == nil || !strings.Contains(err.Error(), "does not support safe expected-identity") {
				t.Fatalf("unexpected error: %v", err)
			}
		})
	}
}

func TestRemoteCapabilityParsingIsAdditive(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\nprintf 'future-capability-v2\\nexpected-identity-v1\\nsession-visibility-v1\\n'\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	if !remoteAgentSupportsCapability(cfg, "expected-identity-v1") ||
		!remoteAgentSupportsCapability(cfg, "session-visibility-v1") ||
		remoteAgentSupportsCapability(cfg, "missing-v1") {
		t.Fatal("newline-delimited capability membership was not respected")
	}
}

func TestRemoteAgentUpdateRequiresCapabilityAndUsesFixedArguments(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities) printf 'expected-identity-v1\\nsigned-self-update-v1\\n'; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\nprintf 'agent is current\\n'\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	argsPath := filepath.Join(dir, "args")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	supported, err := UpdateAgent(context.Background(), cfg)
	if err != nil || !supported {
		t.Fatalf("supported=%t err=%v", supported, err)
	}
	data, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(strings.TrimSpace(string(data)), "\n")
	want := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "update", "--if-newer")
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("agent update args=%v", got)
	}
}

func TestRemoteCreateSendsSessionNameOnStdin(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncat > \"$HMUX_FAKE_STDIN\"\nprintf '$7\\n'\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	argsPath := filepath.Join(dir, "args")
	stdinPath := filepath.Join(dir, "stdin")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_STDIN", stdinPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	inventory := model.Inventory{}

	id, err := Create(context.Background(), cfg, inventory, "codex", "한글 workspace")
	if err != nil || id != "$7" {
		t.Fatalf("id=%q err=%v", id, err)
	}
	argsData, _ := os.ReadFile(argsPath)
	gotArgs := strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "create", "--name-stdin", "codex")
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("create ssh args=%v", gotArgs)
	}
	stdinData, _ := os.ReadFile(stdinPath)
	if string(stdinData) != "한글 workspace\n" {
		t.Fatalf("create stdin=%q", stdinData)
	}
}

func TestRemoteStructuredCreateUsesAuthoritativeAgentResult(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := `#!/bin/sh
set -eu
case "$*" in
*capabilities*) printf 'structured-create-v1\n'; exit 0 ;;
esac
printf '%s\n' "$@" >"$HMUX_FAKE_ARGS"
cat >"$HMUX_FAKE_STDIN"
printf '{"id":"$7","created_at":1700000000,"reused":true}\n'
`
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	argsPath := filepath.Join(dir, "args")
	stdinPath := filepath.Join(dir, "stdin")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_STDIN", stdinPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	result, err := CreateSession(context.Background(), cfg, model.Inventory{}, "codex", "한글 workspace")
	if err != nil {
		t.Fatal(err)
	}
	if result.ID != "$7" || result.CreatedAt != 1700000000 || !result.Reused {
		t.Fatalf("result=%+v", result)
	}
	argsData, _ := os.ReadFile(argsPath)
	gotArgs := strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "create", "--json", "--name-stdin", "codex")
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("create ssh args=%v", gotArgs)
	}
	stdinData, _ := os.ReadFile(stdinPath)
	if string(stdinData) != "한글 workspace\n" {
		t.Fatalf("create stdin=%q", stdinData)
	}
}

func TestRemoteStructuredCreateRejectsInvalidResults(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := `#!/bin/sh
set -eu
case "$*" in
*capabilities*) printf 'structured-create-v1\n'; exit 0 ;;
esac
cat "$HMUX_FAKE_RESPONSE"
`
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	responsePath := filepath.Join(dir, "response")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_RESPONSE", responsePath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	for name, response := range map[string]string{
		"unknown field":  `{"id":"$7","created_at":1,"reused":false,"extra":true}`,
		"trailing data":  `{"id":"$7","created_at":1,"reused":false} {}`,
		"invalid id":     `{"id":"name","created_at":1,"reused":false}`,
		"invalid time":   `{"id":"$7","created_at":0,"reused":false}`,
		"missing reused": `{"id":"$7","created_at":1}`,
		"malformed":      `{"id":`,
	} {
		t.Run(name, func(t *testing.T) {
			if err := os.WriteFile(responsePath, []byte(response), 0o600); err != nil {
				t.Fatal(err)
			}
			if _, err := CreateSession(context.Background(), cfg, model.Inventory{}, "codex", ""); err == nil {
				t.Fatalf("invalid result was accepted: %s", response)
			}
		})
	}
}

func TestMutationCapabilityProbeHonorsCallerCancellation(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	if err := os.WriteFile(sshPath, []byte("#!/bin/sh\nexec sleep 10\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	started := time.Now()
	err := SetAliasExpected(ctx, cfg, "$7", 1700000000, "alias")
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("error=%v", err)
	}
	if time.Since(started) > time.Second {
		t.Fatalf("canceled capability probe took too long: %s", time.Since(started))
	}
}

func TestHiddenExpectedProbesCapabilitiesOnce(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := `#!/bin/sh
set -eu
case "$*" in
*capabilities*)
	printf 'capabilities\n' >>"$HMUX_FAKE_LOG"
	printf 'expected-identity-v1\nsession-visibility-v1\n'
	;;
*)
	printf 'hidden-set\n' >>"$HMUX_FAKE_LOG"
	cat >/dev/null
	;;
esac
`
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	logPath := filepath.Join(dir, "calls")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_LOG", logPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	if err := SetHiddenExpected(context.Background(), cfg, "$7", 1700000000, true); err != nil {
		t.Fatal(err)
	}
	logData, err := os.ReadFile(logPath)
	if err != nil {
		t.Fatal(err)
	}
	if string(logData) != "capabilities\nhidden-set\n" {
		t.Fatalf("calls=%q", logData)
	}
}

func TestRemoteCatalogProtocolMalformedAndTimeout(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\nif [ \"${HMUX_FAKE_SLEEP:-0}\" = 1 ]; then exec sleep 5; fi\ncat \"$HMUX_FAKE_RESPONSE\"\nexit \"${HMUX_FAKE_EXIT:-0}\"\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	responsePath := filepath.Join(dir, "response.json")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_RESPONSE", responsePath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	valid := `{"protocol_version":1,"generated_at":"2026-07-28T00:00:00Z","sessions":[{"id":"$1","name":"ok"}]}`
	if err := os.WriteFile(responsePath, []byte(valid), 0o600); err != nil {
		t.Fatal(err)
	}
	value, err := Catalog(context.Background(), cfg)
	if err != nil || len(value.Sessions) != 1 {
		t.Fatalf("valid remote catalog failed: value=%#v err=%v", value, err)
	}

	mismatch := strings.Replace(valid, `"protocol_version":1`, `"protocol_version":99`, 1)
	if err := os.WriteFile(responsePath, []byte(mismatch), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Catalog(context.Background(), cfg); err == nil || !strings.Contains(err.Error(), "protocol mismatch") {
		t.Fatalf("expected protocol mismatch, got %v", err)
	}

	if err := os.WriteFile(responsePath, []byte(`{"protocol_version":1,"sessions":[`), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Catalog(context.Background(), cfg); err == nil || !strings.Contains(err.Error(), "decode remote catalog") {
		t.Fatalf("expected malformed JSON error, got %v", err)
	}

	t.Setenv("HMUX_FAKE_SLEEP", "1")
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if _, err := Catalog(ctx, cfg); err == nil {
		t.Fatal("expected remote catalog timeout")
	}
}

func TestRemoteCatalogRequestsAndValidatesLauncherLocalTabs(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\ncat \"$HMUX_FAKE_RESPONSE\"\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	argsPath := filepath.Join(dir, "args")
	responsePath := filepath.Join(dir, "response.json")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_FAKE_RESPONSE", responsePath)
	t.Setenv("HMUX_LAUNCHER_ID", "0123456789abcdef0123456789abcdef")
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	response := `{"protocol_version":1,"generated_at":"2026-07-31T00:00:00Z","sessions":[{"id":"$1","name":"one"},{"id":"$2","name":"two"}],"open_tabs":["$2","$1"],"current_tab_id":"$2"}`
	if err := os.WriteFile(responsePath, []byte(response), 0o600); err != nil {
		t.Fatal(err)
	}
	value, err := Catalog(context.Background(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(value.OpenTabs, []string{"$2", "$1"}) ||
		value.CurrentTabID != "$2" {
		t.Fatalf("launcher tabs=%#v current=%q", value.OpenTabs, value.CurrentTabID)
	}
	argsData, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	gotArgs := strings.Split(strings.TrimSpace(string(argsData)), "\n")
	wantArgs := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "catalog", "--launcher", "0123456789abcdef0123456789abcdef")
	if !reflect.DeepEqual(gotArgs, wantArgs) {
		t.Fatalf("catalog ssh args=%v", gotArgs)
	}

	invalid := strings.Replace(response, `"current_tab_id":"$2"`, `"current_tab_id":"$9"`, 1)
	if err := os.WriteFile(responsePath, []byte(invalid), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Catalog(context.Background(), cfg); err == nil {
		t.Fatal("remote catalog accepted a current tab outside open_tabs")
	}
}

func TestRemoteFramedAttachKeepsLauncherAndMapsQuit(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	argsPath := filepath.Join(dir, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\nexit 130\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	t.Setenv("HMUX_LAUNCHER_ID", "0123456789abcdef0123456789abcdef")
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"

	err := Attach(cfg, "$7", false)
	if !errors.Is(err, frame.ErrLauncherExit) {
		t.Fatalf("remote launcher quit=%v", err)
	}
	args, readErr := os.ReadFile(argsPath)
	if readErr != nil {
		t.Fatal(readErr)
	}
	got := strings.Split(strings.TrimSpace(string(args)), "\n")
	want := append([]string{"-tt"}, remoteSSHBaseArgs(cfg.HomeAlias)...)
	want = append(want, cfg.AgentPath, "attach", "--launcher", "0123456789abcdef0123456789abcdef", `\$7`)
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("framed attach args=%v want=%v", got, want)
	}
}

func TestRemoteSessionArgProtectsTmuxIDFromRemoteShellExpansion(t *testing.T) {
	if got := remoteSessionArg("$73"); got != `\$73` {
		t.Fatalf("remote session argument=%q", got)
	}
	output, err := exec.Command("/bin/sh", "-c", "printf '%s' "+remoteSessionArg("$73")).Output()
	if err != nil {
		t.Fatal(err)
	}
	if string(output) != "$73" {
		t.Fatalf("remote shell decoded session argument=%q", output)
	}
}

type appViewRunnerFunc func(context.Context, ...string) ([]byte, error)

func (f appViewRunnerFunc) Output(ctx context.Context, args ...string) ([]byte, error) {
	return f(ctx, args...)
}

func TestExpectedAppViewChecksIdentityAroundGroupedCreation(t *testing.T) {
	var calls [][]string
	runner := appViewRunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		calls = append(calls, append([]string(nil), args...))
		if args[0] == "display-message" {
			return []byte("1700000000\n"), nil
		}
		return nil, nil
	})
	if err := createExpectedAppView(context.Background(), runner, "$8", 1700000000, "hmux-app-view-42-acde"); err != nil {
		t.Fatal(err)
	}
	want := [][]string{
		{"display-message", "-p", "-t", "$8", "#{session_created}"},
		{
			"new-session", "-d", "-s", "hmux-app-view-42-acde", "-t", "$8",
			";", "set-option", "-t", "hmux-app-view-42-acde", "@hmux_app_view", "1",
			";", "set-option", "-t", "hmux-app-view-42-acde", "status", "off",
		},
		{"display-message", "-p", "-t", "$8", "#{session_created}"},
	}
	if !reflect.DeepEqual(calls, want) {
		t.Fatalf("calls=%v want=%v", calls, want)
	}
}

func TestExpectedAppViewCleansOnlyTemporaryViewAfterIdentityChange(t *testing.T) {
	displays := 0
	var calls [][]string
	runner := appViewRunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		calls = append(calls, append([]string(nil), args...))
		if args[0] == "display-message" {
			displays++
			if displays == 1 {
				return []byte("1700000000\n"), nil
			}
			return []byte("1700000001\n"), nil
		}
		return nil, nil
	})
	err := createExpectedAppView(context.Background(), runner, "$8", 1700000000, "hmux-app-view-42-acde")
	if !errors.Is(err, catalog.ErrSessionChanged) {
		t.Fatalf("err=%v", err)
	}
	last := calls[len(calls)-1]
	if !reflect.DeepEqual(last, []string{"kill-session", "-t", "hmux-app-view-42-acde"}) {
		t.Fatalf("cleanup=%v", last)
	}
}

func TestRemoteNativeAppViewUsesAgentInsteadOfBareTmux(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	script := "#!/bin/sh\nset -eu\ncase \"$*\" in *capabilities) printf 'native-app-view-v1\\n'; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$HMUX_FAKE_ARGS\"\n"
	if err := os.WriteFile(sshPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	argsPath := filepath.Join(dir, "args")
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("HMUX_FAKE_ARGS", argsPath)
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	if err := AttachExpectedAppView(cfg, "$8", 1700000000, true); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(strings.TrimSpace(string(data)), "\n")
	want := append([]string{"-tt"}, remoteSSHBaseArgs(cfg.HomeAlias)...)
	want = append(want, cfg.AgentPath, "attach", "--app-view", "--created-at", "1700000000", "--shared", `\$8`)
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("remote native app view args=%v want=%v", got, want)
	}
	for _, argument := range got {
		if argument == "tmux" {
			t.Fatal("remote native app view invoked bare tmux")
		}
	}
}

func TestRemoteNativeAppViewFailsClosedWithoutCapability(t *testing.T) {
	dir := t.TempDir()
	sshPath := filepath.Join(dir, "ssh")
	if err := os.WriteFile(sshPath, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	cfg := config.DefaultClientConfig()
	cfg.Role = "remote"
	err := AttachExpectedAppView(cfg, "$8", 1700000000, false)
	if err == nil || !strings.Contains(err.Error(), "does not support native app views") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestRemoteCatalogValidatesRecoveryLifetime(t *testing.T) {
	for _, prior := range []*model.SessionIdentity{nil, {ID: "$1", CreatedAt: 1}, {ID: "bad", CreatedAt: 1}, {ID: "$1", CreatedAt: 0}} {
		value := model.Catalog{ProtocolVersion: model.ProtocolVersion, Sessions: []model.Session{{ID: "$2", CreatedAt: 2, RestoredFrom: prior}}}
		got, err := normalizeRemoteCatalog(value, config.ClientConfig{HomeAlias: "synthetic-home"}, "")
		valid := prior == nil || (prior.ID == "$1" && prior.CreatedAt == 1)
		if valid && (err != nil || got.Sessions[0].RestoredFrom != prior) {
			t.Fatal("verified recovery lineage lost")
		}
		if !valid && err == nil {
			t.Fatal("invalid recovery lineage accepted")
		}
	}
}

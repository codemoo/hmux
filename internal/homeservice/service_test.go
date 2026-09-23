package homeservice

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"testing"
)

func tempDir(t *testing.T) string {
	t.Helper()
	p, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func sampleSpec() Spec {
	return Spec{Binary: "/Users/test user/bin/hmux-web", Home: "/Users/test user", Endpoint: "wss://hmux.example/connect", TokenFile: "/Users/test user/private/token", ConfigFile: "/Users/test user/.config/hmux/client.toml", LogFile: "/Users/test user/state/service.log", Environment: map[string]string{"HOME": "/Users/test user", "PATH": "/Users/test user/node/bin:/opt/homebrew/bin:/usr/bin:/bin"}}
}

func TestLaunchAgentParsesAndPreservesArguments(t *testing.T) {
	if runtime.GOOS != "darwin" {
		t.Skip("plutil verification requires macOS")
	}
	s := sampleSpec()
	s.TokenFile = "/Users/test user/a&b<quoted>\"/token"
	path := filepath.Join(tempDir(t), "service.plist")
	if err := os.WriteFile(path, LaunchAgent(s), 0600); err != nil {
		t.Fatal(err)
	}
	raw, err := exec.Command("/usr/bin/plutil", "-convert", "json", "-o", "-", path).CombinedOutput()
	if err != nil {
		t.Fatalf("plutil: %v %s", err, raw)
	}
	var decoded struct {
		Label                                     string
		ProgramArguments                          []string
		WorkingDirectory                          string
		RunAtLoad, KeepAlive, AbandonProcessGroup bool
		ThrottleInterval, Umask                   int
	}
	if err := json.Unmarshal(raw, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.Label != Label || !reflect.DeepEqual(decoded.ProgramArguments, s.arguments()) || decoded.WorkingDirectory != s.Home || !decoded.RunAtLoad || !decoded.KeepAlive || !decoded.AbandonProcessGroup || decoded.ThrottleInterval < 5 || decoded.Umask != 63 {
		t.Fatalf("invalid launch agent: %+v", decoded)
	}
	if decoded.ProgramArguments[0] != "/usr/bin/env" || decoded.ProgramArguments[1] != "-i" {
		t.Fatal("manager environment was not cleared")
	}
}

func TestSystemdEscapingAndProcessLifetime(t *testing.T) {
	want := `"/a b/%%n/$$HOME/\"quote\"/back\\slash"`
	if got := unitQuote("/a b/%n/$HOME/\"quote\"/back\\slash", true); got != want {
		t.Fatalf("quote = %s, want %s", got, want)
	}
	if got := unitQuote("/a/%h/$HOME", false); got != `"/a/%%h/$HOME"` {
		t.Fatal(got)
	}
	raw := string(SystemdUnit(sampleSpec()))
	for _, fragment := range []string{"ExecStart=\"/usr/bin/env\" \"-i\"", "Restart=always", "RestartSec=10", "KillMode=process", "UMask=0077", "WantedBy=default.target"} {
		if !strings.Contains(raw, fragment) {
			t.Fatal("missing", fragment)
		}
	}
	if strings.Contains(raw, "/bin/sh") || strings.Contains(raw, "KillMode=control-group") {
		t.Fatal("unsafe process lifecycle")
	}
}

func TestEnvironmentUsesPathsWithoutCopyingSecrets(t *testing.T) {
	t.Setenv("PATH", "/Users/test user/node/bin:/opt/homebrew/bin:/usr/bin:/bin")
	t.Setenv("CODEX_HOME", "/Users/test user/codex")
	t.Setenv("CLAUDE_CONFIG_DIR", "/Users/test user/claude")
	t.Setenv("OPENAI_API_KEY", "test-secret-do-not-copy")
	t.Setenv("SSH_AUTH_SOCK", "/tmp/ephemeral-agent")
	t.Setenv("TMPDIR", "/tmp/ephemeral-login")
	env, err := serviceEnvironment("/Users/test user")
	if err != nil {
		t.Fatal(err)
	}
	if env["PATH"] != os.Getenv("PATH") || env["CODEX_HOME"] != os.Getenv("CODEX_HOME") || env["CLAUDE_CONFIG_DIR"] != os.Getenv("CLAUDE_CONFIG_DIR") {
		t.Fatal("provider paths were lost")
	}
	for _, key := range []string{"OPENAI_API_KEY", "SSH_AUTH_SOCK", "TMPDIR", "TMUX"} {
		if _, ok := env[key]; ok {
			t.Fatal("unexpected environment", key)
		}
	}
	for _, path := range []string{"/bin:", ":/bin", "relative:/bin", "/bin\nInjected=1"} {
		t.Setenv("PATH", path)
		if _, err := serviceEnvironment("/Users/test user"); err == nil {
			t.Fatal("accepted PATH", path)
		}
	}
	t.Setenv("PATH", "/bin")
	t.Setenv("CODEX_HOME", "relative")
	if _, err := serviceEnvironment("/Users/test user"); err == nil {
		t.Fatal("accepted relative provider home")
	}
}

func TestAdoptionRejectsAmbiguousArgumentsAndChangedIdentity(t *testing.T) {
	p := process{PID: 42, Birth: "123:456", Args: []string{"/private/bin/hmux-web", "connect", "--url", "wss://hmux.example/connect", "--token-file", "/private/token", "--config", "/private/client.toml"}}
	endpoint, token, cfg, err := connectorOptions(p)
	if err != nil || endpoint != "wss://hmux.example/connect" || token != "/private/token" || cfg != "/private/client.toml" {
		t.Fatal(endpoint, token, cfg, err)
	}
	for _, change := range []process{
		{PID: 43, Birth: p.Birth, Args: p.Args},
		{PID: p.PID, Birth: "reused", Args: p.Args},
		{PID: p.PID, Birth: p.Birth, Args: append(append([]string{}, p.Args...), "--unknown")},
	} {
		if sameProcess(p, change) {
			t.Fatal("accepted changed process")
		}
	}
	for _, args := range [][]string{
		{"/bin/sh", "connect", "--token-file", "/private/token"},
		{"/bin/hmux-web", "serve", "--token-file", "/private/token"},
		{"/bin/hmux-web", "connect", "--token-file", "relative"},
		{"/bin/hmux-web", "connect", "--token-file", "/private/token", "--unsafe"},
	} {
		if _, _, _, err := connectorOptions(process{Args: args}); err == nil {
			t.Fatal("accepted args", args)
		}
	}
}

func TestServiceManagerUsesUserScopeAndProcessOnlyStops(t *testing.T) {
	for _, mac := range []bool{true, false} {
		var calls [][]string
		loaded := false
		m := manager{processes: func() ([]process, error) { return nil, nil }, mac: mac, command: "manager", domain: "gui/501", target: "gui/501/" + Label, path: "/private/test-service", execute: func(_ context.Context, name string, args ...string) ([]byte, error) {
			calls = append(calls, args)
			if len(args) > 0 && args[0] == "print" && !loaded {
				return nil, errors.New("not loaded")
			}
			return nil, nil
		}}
		if err := m.start(context.Background()); err != nil {
			t.Fatal(err)
		}
		var want [][]string
		if mac {
			want = [][]string{{"enable", m.target}, {"print", m.target}, {"bootstrap", m.domain, m.path}}
		} else {
			want = [][]string{{"--user", "daemon-reload"}, {"--user", "enable", "--now", Unit}}
		}
		if !reflect.DeepEqual(calls, want) {
			t.Fatalf("start: %v", calls)
		}
		calls = nil
		loaded = true
		if err := m.stop(context.Background(), true); err != nil {
			t.Fatal(err)
		}
		if mac {
			want = [][]string{{"disable", m.target}, {"print", m.target}, {"bootout", m.target}}
		} else {
			want = [][]string{{"--user", "disable", "--now", Unit}}
		}
		if !reflect.DeepEqual(calls, want) {
			t.Fatalf("stop: %v", calls)
		}
	}
}

func TestServiceManagerFailureIsReturnedBeforeStarting(t *testing.T) {
	var calls int
	m := manager{processes: func() ([]process, error) { return nil, nil }, execute: func(context.Context, string, ...string) ([]byte, error) {
		calls++
		return nil, errors.New("manager unavailable")
	}}
	if err := m.start(context.Background()); err == nil || calls != 1 {
		t.Fatal(err, calls)
	}
}

func TestBackupAndSymlinkRefusal(t *testing.T) {
	root := tempDir(t)
	file := filepath.Join(root, "service")
	if err := writeBackedUp(file, []byte("old"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := writeBackedUp(file, []byte("new"), 0600); err != nil {
		t.Fatal(err)
	}
	backups, _ := filepath.Glob(file + ".hmux-backup-*")
	if len(backups) != 1 {
		t.Fatal(backups)
	}
	raw, _ := os.ReadFile(backups[0])
	if string(raw) != "old" {
		t.Fatal(string(raw))
	}
	link := filepath.Join(root, "link")
	if err := os.Symlink(file, link); err != nil {
		t.Fatal(err)
	}
	if err := writeBackedUp(link, []byte("bad"), 0600); err == nil {
		t.Fatal("followed symlink")
	}
	raw, _ = os.ReadFile(file)
	if string(raw) != "new" {
		t.Fatal(string(raw))
	}
	if _, err := readOwned(file, 2, true); err == nil {
		t.Fatal("accepted oversized file")
	}
	if err := os.Chmod(file, 0644); err != nil {
		t.Fatal(err)
	}
	if _, err := readOwned(file, 1024, true); err == nil {
		t.Fatal("accepted readable secret")
	}
	if err := os.Link(file, filepath.Join(root, "hardlink")); err != nil {
		t.Fatal(err)
	}
	if _, err := readOwned(file, 1024, false); err == nil {
		t.Fatal("accepted hardlink")
	}
}

func TestConnectorLockSurvivesOtherOpenAndReleases(t *testing.T) {
	dir := tempDir(t)
	first, err := LockConnector(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	if second, err := LockConnector(dir); err == nil {
		second.Close()
		t.Fatal("duplicate connector allowed")
	}
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}
	third, err := LockConnector(dir)
	if err != nil {
		t.Fatal(err)
	}
	third.Close()
	path := filepath.Join(dir, "home-connector.lock")
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(dir, "private")
	if err := os.WriteFile(target, []byte("keep"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, path); err != nil {
		t.Fatal(err)
	}
	if f, err := LockConnector(dir); err == nil {
		f.Close()
		t.Fatal("followed lock symlink")
	}
	raw, _ := os.ReadFile(target)
	if string(raw) != "keep" {
		t.Fatal(string(raw))
	}
}

func TestLogsRotateWithinBoundAndRejectSymlinks(t *testing.T) {
	dir := tempDir(t)
	path := filepath.Join(dir, "service.log")
	l, err := OpenLog(path)
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	chunk := bytes.Repeat([]byte("a"), LogLimit/2)
	for i := 0; i < 7; i++ {
		if _, err := l.Write(chunk); err != nil {
			t.Fatal(err)
		}
	}
	entries, err := os.ReadDir(dir)
	if err != nil || len(entries) != 2 {
		t.Fatal(entries, err)
	}
	for _, entry := range entries {
		info, _ := entry.Info()
		if info.Size() > LogLimit || info.Mode().Perm() != 0600 {
			t.Fatal(info)
		}
	}
	if _, err := l.Write(bytes.Repeat([]byte("x"), LogLimit+1)); err == nil {
		t.Fatal("oversized log write")
	}
	l.Close()
	if _, err := l.Write([]byte("after close")); !errors.Is(err, os.ErrClosed) {
		t.Fatal(err)
	}
	link := filepath.Join(dir, "link")
	if err := os.Symlink(path, link); err != nil {
		t.Fatal(err)
	}
	if log, err := OpenLog(link); err == nil {
		log.Close()
		t.Fatal("followed log symlink")
	}
}

func TestGUIAvailabilityDiscardsLargeDomainListing(t *testing.T) {
	p := filepath.Join(tempDir(t), "manager")
	if err := os.WriteFile(p, []byte("#!/bin/sh\nhead -c 262144 /dev/zero\n"), 0700); err != nil {
		t.Fatal(err)
	}
	m := manager{mac: true, command: p, domain: "gui/123"}
	if err := m.available(context.Background()); err != nil {
		t.Fatal(err)
	}
}
func TestGUIAvailabilityDoesNotAssumeLoggedOut(t *testing.T) {
	m := manager{mac: true, execute: func(context.Context, string, ...string) ([]byte, error) { return nil, errors.New("access denied") }}
	err := m.available(context.Background())
	if err == nil || !strings.Contains(err.Error(), "sandbox") || !strings.Contains(err.Error(), "access denied") {
		t.Fatal(err)
	}
}

func TestSystemdWorkingDirectoryIsScalarPath(t *testing.T) {
	s := sampleSpec()
	s.Home = "/home/example space%name"
	raw := string(SystemdUnit(s))
	if !strings.Contains(raw, "\nWorkingDirectory=/home/example space%%name\n") {
		t.Fatal("working directory must not use ExecStart quotes")
	}
}

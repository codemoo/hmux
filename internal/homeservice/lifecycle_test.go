package homeservice

import (
	"context"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"syscall"
	"testing"
	"time"
)

func TestProcessEnvironmentFiltersSecretsAndKeepsOriginalPaths(t *testing.T) {
	t.Setenv("PATH", "/installation/bin:/usr/bin:/bin")
	t.Setenv("USER", "forged-installer-user")
	raw := []byte("HOME=/Users/example\x00PATH=/original/node/bin:/usr/bin:/bin\x00CODEX_HOME=/original/codex\x00OPENAI_API_KEY=private-test-value\x00TMPDIR=/temporary/login\x00PATH=/duplicate/ignored\x00")
	captured := processEnvironment(raw)
	if len(captured) != 3 || captured["PATH"] != "/original/node/bin:/usr/bin:/bin" {
		t.Fatal(captured)
	}
	env, err := serviceEnvironmentFrom("/Users/example", captured)
	if err != nil {
		t.Fatal(err)
	}
	if env["PATH"] != captured["PATH"] || env["CODEX_HOME"] != "/original/codex" || env["USER"] == "forged-installer-user" || env["USER"] == "" || env["USER"] != env["LOGNAME"] {
		t.Fatal("incorrect environment adoption")
	}
	s := sampleSpec()
	s.Environment = env
	for _, rendered := range [][]byte{LaunchAgent(s), SystemdUnit(s)} {
		for _, value := range []string{"private-test-value", "/installation/bin", "/temporary/login", "OPENAI_API_KEY"} {
			if strings.Contains(string(rendered), value) {
				t.Fatal("unexpected environment in service definition", value)
			}
		}
	}
	if _, err := serviceEnvironmentFrom("/other/home", captured); err == nil {
		t.Fatal("accepted another HOME")
	}
}

func TestConnectorLockHelper(t *testing.T) {
	if os.Getenv("HMUX_TEST_LOCK_CHILD") != "1" {
		return
	}
	f, err := LockConnector(os.Getenv("HMUX_TEST_LOCK_DIR"))
	if f != nil {
		defer f.Close()
	}
	if (err != nil) != (os.Getenv("HMUX_TEST_LOCK_BUSY") == "1") {
		t.Fatal("unexpected cross-process lock result", err)
	}
}

func TestConnectorLockAcrossProcesses(t *testing.T) {
	dir := tempDir(t)
	f, err := LockConnector(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	bin, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	probe := func(busy string) {
		t.Helper()
		cmd := exec.Command(bin, "-test.run=^TestConnectorLockHelper$")
		cmd.Env = append(os.Environ(), "HMUX_TEST_LOCK_CHILD=1", "HMUX_TEST_LOCK_DIR="+dir, "HMUX_TEST_LOCK_BUSY="+busy)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("child: %v %s", err, out)
		}
	}
	probe("1")
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	probe("0")
}

func testChild(t *testing.T, duration ...string) *exec.Cmd {
	t.Helper()
	seconds := "60"
	if len(duration) > 0 {
		seconds = duration[0]
	}
	cmd := exec.Command("/bin/sleep", seconds)
	cmd.Env = append(os.Environ(), "HMUX_TEST_SECRET=must-not-be-captured")
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = cmd.Process.Kill(); _ = cmd.Wait() })
	return cmd
}

func TestVerifiedStopOnlySignalsExactDisposableChild(t *testing.T) {
	child := testChild(t)
	// Linux start times have tick resolution: simultaneous children may share
	// a birth tick, so give the unrelated process distinct arguments as well.
	other := testChild(t, "61")
	p, err := readProcess(child.Process.Pid)
	if errors.Is(err, syscall.EPERM) || errors.Is(err, syscall.EACCES) {
		t.Skip("sandbox denies disposable process metadata")
	}
	if err != nil {
		t.Fatal(err)
	}
	if p.Environment["HMUX_TEST_SECRET"] != "" {
		t.Fatal("captured a non-allowlisted value")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	changed := p
	changed.Birth = "different"
	if err := stopAdopted(ctx, changed); err == nil {
		t.Fatal("signalled changed start time")
	}
	changed = p
	changed.PID = other.Process.Pid
	if err := stopAdopted(ctx, changed); err == nil {
		t.Fatal("signalled different PID")
	}
	changed = p
	changed.Args = append(append([]string{}, p.Args...), "different")
	if err := stopAdopted(ctx, changed); err == nil {
		t.Fatal("signalled changed arguments")
	}
	if err := child.Process.Signal(syscall.Signal(0)); err != nil {
		t.Fatal("child stopped before verified handoff")
	}
	if err := stopAdopted(ctx, p); err != nil {
		t.Fatal(err)
	}
	_ = child.Wait()
	if err := other.Process.Signal(syscall.Signal(0)); err != nil {
		t.Fatal("unrelated process was stopped")
	}
}

func TestStartRefusesUnmanagedConnectorBeforeMutatingManager(t *testing.T) {
	var calls [][]string
	m := manager{processes: func() ([]process, error) { return []process{{PID: 42}}, nil }, execute: func(_ context.Context, _ string, args ...string) ([]byte, error) {
		calls = append(calls, args)
		return []byte("0\n"), nil
	}}
	if err := m.start(context.Background()); err == nil {
		t.Fatal("started competing connector")
	}
	if !reflect.DeepEqual(calls, [][]string{{"--user", "show", "--property=MainPID", "--value", Unit}}) {
		t.Fatal(calls)
	}
}

func TestRestartWaitsForOldProcessExit(t *testing.T) {
	reads := 0
	m := manager{processes: func() ([]process, error) {
		reads++
		if reads == 1 {
			return []process{{PID: 42}}, nil
		}
		return nil, nil
	}}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := m.waitForStop(ctx); err != nil || reads != 2 {
		t.Fatal(err, reads)
	}
	m.processes = func() ([]process, error) { return []process{{PID: 42}}, nil }
	canceled, stop := context.WithCancel(context.Background())
	stop()
	if err := m.waitForStop(canceled); !errors.Is(err, context.Canceled) {
		t.Fatal(err)
	}
}

func TestSystemdNativeVerifier(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("systemd verifier requires Linux")
	}
	bin, err := exec.LookPath("systemd-analyze")
	if err != nil {
		t.Skip("systemd-analyze unavailable")
	}
	s := sampleSpec()
	s.TokenFile = "/private/a b%h$HOME/token"
	path := filepath.Join(tempDir(t), Unit)
	raw := SystemdUnit(s)
	if strings.Contains(string(raw), "journal") {
		t.Fatal("unbounded journal output")
	}
	if err := os.WriteFile(path, raw, 0600); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "verify", "--man=no", path)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("systemd verifier: %v %s", err, out)
	}
}

// Compile-time contract: fixed lifecycle logging accepts the ordinary writer
// interface, without a resident logging process or buffering terminal output.
var _ io.WriteCloser = (*Log)(nil)

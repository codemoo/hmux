package frame

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"
)

func TestFramePaneIDValidation(t *testing.T) {
	for _, value := range []string{"%0", "%42", "%123456789"} {
		if !validPaneID(value) {
			t.Fatalf("valid pane ID rejected: %q", value)
		}
	}
	for _, value := range []string{"", "0", "%", "%-1", "%1;kill-server", "%１２"} {
		if validPaneID(value) {
			t.Fatalf("invalid pane ID accepted: %q", value)
		}
	}
}

func TestDisposableBodyRoutingValidation(t *testing.T) {
	const launcher = "0123456789abcdef0123456789abcdef"
	valid := "/tmp/tmux-501/hmux-frame-ui-0123456789ab-4321,99,0"
	if err := validateBodyTMUXRouting(valid, launcher); err != nil {
		t.Fatal(err)
	}
	for _, value := range []string{
		"/tmp/tmux-501/default,99,0",
		"/tmp/tmux-501/hmux-frame-ui-ffffffffffff-4321,99,0",
		"/tmp/tmux-501/hmux-frame-ui-0123456789ab-bad,99,0",
	} {
		if err := validateBodyTMUXRouting(value, launcher); err == nil {
			t.Fatalf("unsafe body routing accepted: %q", value)
		}
	}
}

func TestInnerEnvironmentRemovesOnlyOuterTmuxRouting(t *testing.T) {
	input := []string{
		"PATH=/usr/bin",
		"TMUX=/tmp/outer,1,0",
		"TMUX_PANE=%1",
		"TERM=tmux-256color",
	}
	want := []string{
		"PATH=/usr/bin",
		"TERM=tmux-256color",
		"HMUX_FRAMED=1",
	}
	if got := innerEnvironment(input); !reflect.DeepEqual(got, want) {
		t.Fatalf("environment=%v want=%v", got, want)
	}
}

func TestFrameCommandEnvironmentReplacesStaleFrameValues(t *testing.T) {
	input := []string{
		"PATH=/usr/bin",
		envLauncher + "=stale",
		envSession + "=$999",
	}
	replacements := map[string]string{
		envClient: "0", envLauncher: "0123456789abcdef0123456789abcdef",
		envSession: "$2", envStateDir: "/tmp/state",
		envStatusFile: "/tmp/state/status", envUIConfig: "/tmp/frame-ui.conf",
	}
	got := frameCommandEnvironment(input, replacements)
	if strings.Contains(strings.Join(got, "\n"), "stale") ||
		strings.Contains(strings.Join(got, "\n"), "$999") {
		t.Fatalf("stale frame environment survived: %v", got)
	}
	for key, value := range replacements {
		want := key + "=" + value
		count := 0
		for _, entry := range got {
			if entry == want {
				count++
			}
		}
		if count != 1 {
			t.Fatalf("%s count=%d in %v", want, count, got)
		}
	}
}

func TestPrivateDecoderInterceptsOnlyCompleteHmuxSequences(t *testing.T) {
	decoder := &privateDecoder{}
	var decoded []decodedInput
	decoded = append(decoded, decoder.Feed([]byte("abc\x1b[5;90"))...)
	decoded = append(decoded, decoder.Feed([]byte("21~xyz\x1b[A"))...)
	var plain []byte
	var actions []string
	for _, item := range decoded {
		plain = append(plain, item.data...)
		if item.action != "" {
			actions = append(actions, item.action)
		}
	}
	plain = append(plain, decoder.Flush()...)
	if string(plain) != "abcxyz\x1b[A" {
		t.Fatalf("plain=%q", plain)
	}
	if !reflect.DeepEqual(actions, []string{"1"}) {
		t.Fatalf("actions=%v", actions)
	}
}

func TestPrivateDecoderFlushesStandaloneEscape(t *testing.T) {
	decoder := &privateDecoder{}
	if got := decoder.Feed([]byte{0x1b}); len(got) != 0 {
		t.Fatalf("standalone escape was emitted before timeout: %#v", got)
	}
	if got := decoder.Flush(); !reflect.DeepEqual(got, []byte{0x1b}) {
		t.Fatalf("flushed=%v", got)
	}
}

func TestAliasEditorKeepsBytesAfterRejectedEnter(t *testing.T) {
	editor := &aliasEditor{active: true, value: []byte("alias")}
	remainder, finished := handleAliasInput(
		editor,
		append([]byte{'\r'}, []byte("next")...),
		environment{},
		"/dev/ttys777",
		80,
		24,
	)
	// An empty environment cannot persist the alias, so Enter remains in edit
	// mode and later bytes are consumed as alias input instead of leaking.
	if finished || remainder != nil || string(editor.value) != "aliasnext" {
		t.Fatalf("finished=%t remainder=%q value=%q", finished, remainder, editor.value)
	}
}

func TestValidateOptionsRejectsSymlinkedFrameConfigs(t *testing.T) {
	root := t.TempDir()
	realConfig := filepath.Join(root, "real.conf")
	if err := os.WriteFile(realConfig, []byte("set -g status off\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(root, "frame.conf")
	if err := os.Symlink(realConfig, link); err != nil {
		t.Fatal(err)
	}
	for _, options := range []Options{
		{
			StateDir: filepath.Join(root, "state"), ConfigPath: link,
			UIConfigPath: realConfig,
			LauncherID:   "0123456789abcdef0123456789abcdef",
			SessionID:    "$1",
		},
		{
			StateDir: filepath.Join(root, "state"), ConfigPath: realConfig,
			UIConfigPath: link,
			LauncherID:   "0123456789abcdef0123456789abcdef",
			SessionID:    "$1",
		},
	} {
		if err := validateOptions(options); err == nil {
			t.Fatal("symlinked frame config was accepted")
		}
	}
}

func TestFrameClickStatusPathRequiresLauncherBoundPrivateFilename(t *testing.T) {
	stateDir := t.TempDir()
	launcher := "0123456789abcdef0123456789abcdef"
	valid := filepath.Join(stateDir, "frames", launcher+"-321.status")
	if err := validateFrameStatusPath(stateDir, launcher, valid); err != nil {
		t.Fatalf("valid status path: %v", err)
	}
	for _, invalid := range []string{
		filepath.Join(stateDir, "frames", "fedcba9876543210fedcba9876543210-321.status"),
		filepath.Join(stateDir, "frames", launcher+"-bad.status"),
		filepath.Join(stateDir, "frames", launcher+"-1.status"),
		filepath.Join(stateDir, "outside", launcher+"-321.status"),
	} {
		if err := validateFrameStatusPath(stateDir, launcher, invalid); err == nil {
			t.Fatalf("invalid status path accepted: %q", invalid)
		}
	}
}

func TestCommandStatusPreservesOnlyLauncherExitSignal(t *testing.T) {
	if got := commandStatus(nil); got != 0 {
		t.Fatalf("success status=%d", got)
	}
	command := execExitCommand(t, 130)
	if got := commandStatus(command); got != 130 {
		t.Fatalf("cancel status=%d", got)
	}
	command = execExitCommand(t, 7)
	if got := commandStatus(command); got != 1 {
		t.Fatalf("failure status=%d", got)
	}
}

func TestRunFrontendStopsDisposableClientWhenStatusIsReady(t *testing.T) {
	statusFile := filepath.Join(t.TempDir(), "frame.status")
	command := exec.Command("/bin/sh", "-c", "sleep 10")
	writeDone := make(chan error, 1)
	go func() {
		time.Sleep(20 * time.Millisecond)
		writeDone <- writeStatus(statusFile, 0)
	}()
	started := time.Now()
	runErr, stopped := runFrontend(command, statusFile)
	if err := <-writeDone; err != nil {
		t.Fatal(err)
	}
	if !stopped || runErr == nil {
		t.Fatalf("stopped=%t err=%v", stopped, runErr)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("frontend stop took %s", elapsed)
	}
	if status, ready := readReadyStatus(statusFile); !ready || status != 0 {
		t.Fatalf("status=%d ready=%t", status, ready)
	}
}

func TestRecordFailurePublishesSanitizedFirstCause(t *testing.T) {
	statusFile := filepath.Join(t.TempDir(), "frame.status")
	first := errors.New("target\x1b[31m failed\nwith details")
	if err := recordFailure(statusFile, first); !errors.Is(err, first) {
		t.Fatalf("record failure=%v", err)
	}
	if status, ready := readReadyStatus(statusFile); !ready || status != 1 {
		t.Fatalf("status=%d ready=%t", status, ready)
	}
	detail, ok := readFailure(statusFile)
	if !ok || detail != "target [31m failed with details" {
		t.Fatalf("detail=%q ready=%t", detail, ok)
	}
	second := errors.New("less specific wrapper")
	if err := recordFailure(statusFile, second); !errors.Is(err, second) {
		t.Fatalf("second record failure=%v", err)
	}
	if detail, ok = readFailure(statusFile); !ok || detail != "target [31m failed with details" {
		t.Fatalf("first detail was overwritten: %q ready=%t", detail, ok)
	}
}

func execExitCommand(t *testing.T, status int) error {
	t.Helper()
	command := exec.Command("/bin/sh", "-c", "exit "+strconv.Itoa(status))
	return command.Run()
}

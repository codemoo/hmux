package catalog

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

type fakeRunner struct {
	calls    int
	commands [][]string
}

func (f *fakeRunner) Output(_ context.Context, args ...string) ([]byte, error) {
	f.calls++
	f.commands = append(f.commands, append([]string(nil), args...))
	switch args[0] {
	case "list-sessions":
		return []byte(strings.Join([]string{
			"$7", "코덱 main\x1b[31m", "1700000000", "1700000200", "1", "2", "", "",
		}, separator) + "\n"), nil
	case "list-windows":
		return []byte(
			strings.Join([]string{"$7", "editor", "1", "/tmp/긴 경로", "codex", "120", "40", ""}, separator) + "\n" +
				strings.Join([]string{"$7", "logs", "0", "/tmp", "tail", "80", "24", ""}, separator) + "\n",
		), nil
	default:
		return nil, errors.New("unexpected command")
	}
}

func TestReadUsesTwoTmuxCallsAndParsesCatalog(t *testing.T) {
	runner := &fakeRunner{}
	value, err := Read(context.Background(), runner)
	if err != nil {
		t.Fatal(err)
	}
	if runner.calls != 2 {
		t.Fatalf("calls=%d", runner.calls)
	}
	sessionCommand := strings.Join(runner.commands[0], " ")
	windowCommand := strings.Join(runner.commands[1], " ")
	if !strings.Contains(sessionCommand, "session_group_attached") ||
		strings.Contains(sessionCommand, "session_width") ||
		!strings.Contains(windowCommand, "window_width") {
		t.Fatalf("incorrect tmux dimension formats: sessions=%q windows=%q", sessionCommand, windowCommand)
	}
	if len(value.Sessions) != 1 {
		t.Fatalf("sessions=%d", len(value.Sessions))
	}
	session := value.Sessions[0]
	if session.ID != "$7" || session.Runtime != "process" || session.CurrentPath != "/tmp/긴 경로" ||
		session.Width != 120 || session.Height != 40 || session.Attached != 1 || session.Alias != "" {
		t.Fatalf("unexpected session: %#v", session)
	}
	if strings.Contains(session.Name, "\x1b") {
		t.Fatal("ANSI escape was not stripped")
	}
	if !reflect.DeepEqual(session.WindowNames, []string{"editor", "logs"}) {
		t.Fatalf("windows=%v", session.WindowNames)
	}
}

func TestReadTreatsOnlyMissingTmuxServerAsAnEmptyCatalog(t *testing.T) {
	dir := t.TempDir()
	tmuxPath := filepath.Join(dir, "tmux")
	script := "#!/bin/sh\nprintf '%s\\n' 'error connecting to /tmp/tmux-test (No such file or directory)' >&2\nexit 1\n"
	if err := os.WriteFile(tmuxPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	value, err := ReadBasic(context.Background(), TmuxRunner{Path: tmuxPath})
	if err != nil {
		t.Fatal(err)
	}
	if value.ProtocolVersion != model.ProtocolVersion || len(value.Sessions) != 0 ||
		value.GeneratedAt.IsZero() || value.Sessions == nil {
		t.Fatalf("empty catalog=%#v", value)
	}

	script = "#!/bin/sh\nprintf '%s\\n' 'permission denied' >&2\nexit 1\n"
	if err := os.WriteFile(tmuxPath, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := ReadBasic(context.Background(), TmuxRunner{Path: tmuxPath}); err == nil {
		t.Fatal("non-empty-server failure was hidden")
	}
}

func TestEmptyCatalogSerializesSessionsAsArray(t *testing.T) {
	runner := RunnerFunc(func(context.Context, ...string) ([]byte, error) { return nil, nil })
	value, err := ReadBasic(context.Background(), runner)
	if err != nil {
		t.Fatal(err)
	}
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), `"sessions":[]`) {
		t.Fatalf("empty catalog breaks typed clients: %s", data)
	}
}

func TestReadRejectsMalformedOutput(t *testing.T) {
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		if args[0] == "list-sessions" {
			return []byte("malformed\n"), nil
		}
		return nil, nil
	})
	if _, err := Read(context.Background(), runner); err == nil {
		t.Fatal("expected malformed row error")
	}
}

func TestReadRejectsInvalidNumericFields(t *testing.T) {
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		if args[0] == "list-sessions" {
			return []byte(strings.Join([]string{
				"$7", "name", "not-a-number", "1700000200", "0", "1", "", "",
			}, separator) + "\n"), nil
		}
		return nil, nil
	})
	if _, err := Read(context.Background(), runner); err == nil {
		t.Fatal("expected invalid numeric field error")
	}
}

func TestReadAcceptsUnavailableWindowDimensions(t *testing.T) {
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		if args[0] == "list-sessions" {
			return []byte(strings.Join([]string{
				"$7", "name", "1700000000", "1700000200", "0", "1", "", "",
			}, separator) + "\n"), nil
		}
		return []byte(strings.Join([]string{
			"$7", "window", "1", "/tmp", "zsh", "", "", "",
		}, separator) + "\n"), nil
	})
	value, err := Read(context.Background(), runner)
	if err != nil {
		t.Fatal(err)
	}
	if len(value.Sessions) != 1 || value.Sessions[0].Width != 0 || value.Sessions[0].Height != 0 {
		t.Fatalf("unexpected catalog: %#v", value)
	}
}

type RunnerFunc func(context.Context, ...string) ([]byte, error)

func (f RunnerFunc) Output(ctx context.Context, args ...string) ([]byte, error) {
	return f(ctx, args...)
}

func FuzzReadNeverPanics(f *testing.F) {
	f.Add([]byte("malformed\n"), []byte(""))
	f.Add(
		[]byte(strings.Join([]string{
			"$1", "name", "1700000000", "1700000200", "0", "1", "", "",
		}, separator)+"\n"),
		[]byte(strings.Join([]string{
			"$1", "window", "1", "/tmp", "zsh", "80", "24", "",
		}, separator)+"\n"),
	)
	f.Fuzz(func(t *testing.T, sessions, windows []byte) {
		runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
			if args[0] == "list-sessions" {
				return sessions, nil
			}
			return windows, nil
		})
		_, _ = Read(context.Background(), runner)
	})
}

func TestAttachArgs(t *testing.T) {
	args, err := AttachArgs("$9", false, false)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(args, []string{"attach-session", "-d", "-t", "$9"}) {
		t.Fatalf("args=%v", args)
	}
	args, _ = AttachArgs("$9", true, false)
	if !reflect.DeepEqual(args, []string{"attach-session", "-t", "$9"}) {
		t.Fatalf("shared args=%v", args)
	}
	args, _ = AttachArgs("$9", false, true)
	if !reflect.DeepEqual(args, []string{"switch-client", "-t", "$9"}) {
		t.Fatalf("nested args=%v", args)
	}
	if _, err := AttachArgs("$9;rm", false, false); err == nil {
		t.Fatal("unsafe id accepted")
	}
}

func TestReadHidesOnlyMarkedNativeAppViews(t *testing.T) {
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		if args[0] == "list-sessions" {
			return []byte(strings.Join([]string{
				strings.Join([]string{"$7", "real", "1700000000", "1700000200", "0", "1", "", "1"}, separator),
				strings.Join([]string{"$8", "hmux-app-view-test", "1700000001", "1700000200", "1", "1", "1", "1"}, separator),
			}, "\n") + "\n"), nil
		}
		return []byte(strings.Join([]string{
			"$7", "window", "1", "/tmp", "zsh", "80", "24", "",
		}, separator) + "\n"), nil
	})
	value, err := ReadBasic(context.Background(), runner)
	if err != nil {
		t.Fatal(err)
	}
	if len(value.Sessions) != 1 || value.Sessions[0].ID != "$7" || value.Sessions[0].Attached != 1 {
		t.Fatalf("catalog=%#v", value)
	}
}

func TestReadUsesGroupedAttachmentCountForNativeViews(t *testing.T) {
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		if args[0] == "list-sessions" {
			return []byte(strings.Join([]string{
				strings.Join([]string{"$7", "real", "1700000000", "1700000200", "0", "1", "", "2"}, separator),
				strings.Join([]string{"$8", "hmux-app-view-one", "1700000001", "1700000200", "1", "1", "1", "2"}, separator),
				strings.Join([]string{"$9", "hmux-app-view-two", "1700000002", "1700000200", "1", "1", "1", "2"}, separator),
			}, "\n") + "\n"), nil
		}
		return []byte(strings.Join([]string{
			"$7", "window", "1", "/tmp", "zsh", "80", "24", "",
		}, separator) + "\n"), nil
	})
	value, err := ReadBasic(context.Background(), runner)
	if err != nil {
		t.Fatal(err)
	}
	if len(value.Sessions) != 1 || value.Sessions[0].Attached != 2 {
		t.Fatalf("catalog=%#v", value)
	}
}

func TestReadRejectsInvalidGroupedAttachmentCount(t *testing.T) {
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		if args[0] == "list-sessions" {
			return []byte(strings.Join([]string{
				"$7", "real", "1700000000", "1700000200", "0", "1", "", "invalid",
			}, separator) + "\n"), nil
		}
		return nil, nil
	})
	if _, err := ReadBasic(context.Background(), runner); err == nil {
		t.Fatal("expected invalid session_group_attached error")
	}
}

func TestAppViewArgumentsAreScopedAndValidated(t *testing.T) {
	got, err := AppViewCreateArgs("$8", "hmux-app-view-42-acde")
	if err != nil {
		t.Fatal(err)
	}
	want := []string{
		"new-session", "-d", "-s", "hmux-app-view-42-acde", "-t", "$8",
		";", "set-option", "-t", "hmux-app-view-42-acde", "@hmux_app_view", "1",
		";", "set-option", "-t", "hmux-app-view-42-acde", "status", "off",
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("create args=%v", got)
	}
	if _, err := AppViewCreateArgs("$8;bad", "hmux-app-view-42-acde"); err == nil {
		t.Fatal("unsafe target accepted")
	}
	if _, err := AppViewCreateArgs("$8", "other"); err == nil {
		t.Fatal("unsafe app view accepted")
	}
	attach, _ := AppViewAttachArgs("hmux-app-view-42-acde", false)
	if !reflect.DeepEqual(attach, []string{"attach-session", "-d", "-t", "hmux-app-view-42-acde"}) {
		t.Fatalf("attach args=%v", attach)
	}
	kill, _ := AppViewKillArgs("hmux-app-view-42-acde")
	if !reflect.DeepEqual(kill, []string{"kill-session", "-t", "hmux-app-view-42-acde"}) {
		t.Fatalf("kill args=%v", kill)
	}
}

func TestCurrentSessionIDUsesAValidatedArgumentArray(t *testing.T) {
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		want := []string{"display-message", "-p", "#{session_id}"}
		if !reflect.DeepEqual(args, want) {
			return nil, fmt.Errorf("args=%v", args)
		}
		return []byte("$7\n"), nil
	})
	id, err := CurrentSessionID(context.Background(), runner)
	if err != nil || id != "$7" {
		t.Fatalf("id=%q err=%v", id, err)
	}
}

func TestTargetedClientActionsUseValidatedArgumentArrays(t *testing.T) {
	var calls [][]string
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		calls = append(calls, append([]string(nil), args...))
		return nil, nil
	})
	if err := SwitchClient(context.Background(), runner, "/dev/ttys101", "$8"); err != nil {
		t.Fatal(err)
	}
	want := [][]string{
		{"switch-client", "-c", "/dev/ttys101", "-t", "$8"},
	}
	if !reflect.DeepEqual(calls, want) {
		t.Fatalf("calls=%v", calls)
	}
	if err := SwitchClient(context.Background(), runner, "bad;client", "$8"); err == nil {
		t.Fatal("unsafe client accepted")
	}
}

func TestTerminationUsesValidatedArgumentArray(t *testing.T) {
	var calls [][]string
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		calls = append(calls, append([]string(nil), args...))
		return nil, nil
	})
	if err := TerminateSession(context.Background(), runner, "$8"); err != nil {
		t.Fatal(err)
	}
	want := [][]string{
		{"kill-session", "-t", "$8"},
	}
	if !reflect.DeepEqual(calls, want) {
		t.Fatalf("calls=%v", calls)
	}
	for _, invalid := range []string{"$8;bad", "8", ""} {
		if err := TerminateSession(context.Background(), runner, invalid); err == nil {
			t.Fatalf("unsafe termination target %q accepted", invalid)
		}
	}
}

func TestExpectedTerminationChecksIdentityInOneTmuxCommand(t *testing.T) {
	var calls [][]string
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		calls = append(calls, append([]string(nil), args...))
		return nil, nil
	})
	if err := TerminateSessionExpected(context.Background(), runner, "$8", 1700000000); err != nil {
		t.Fatal(err)
	}
	want := [][]string{{
		"if-shell", "-F", "-t", "$8", "#{==:#{session_created},1700000000}",
		"kill-session -t $8", "display-message -p hmux-session-changed",
	}}
	if !reflect.DeepEqual(calls, want) {
		t.Fatalf("calls=%v", calls)
	}

	changed := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		return []byte("hmux-session-changed\n"), nil
	})
	if err := TerminateSessionExpected(context.Background(), changed, "$8", 1700000000); !errors.Is(err, ErrSessionChanged) {
		t.Fatalf("expected identity error, got %v", err)
	}
}

func TestExpectedAttachChecksIdentityInOneTmuxCommand(t *testing.T) {
	got, err := AttachExpectedArgs("$8", 1700000000, true)
	if err != nil {
		t.Fatal(err)
	}
	want := []string{
		"if-shell", "-F", "-t", "$8", "#{==:#{session_created},1700000000}",
		"attach-session -t $8", "display-message -p hmux-session-changed",
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("args=%v", got)
	}

	got, err = AttachExpectedArgs("$8", 1700000000, false)
	if err != nil {
		t.Fatal(err)
	}
	want = []string{
		"if-shell", "-F", "-t", "$8", "#{==:#{session_created},1700000000}",
		"attach-session -d -t $8", "display-message -p hmux-session-changed",
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("handoff args=%v", got)
	}
}

func TestLegacyMetadataMigrationUsesOnlyValidatedOptionUnsets(t *testing.T) {
	var calls [][]string
	runner := RunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		calls = append(calls, append([]string(nil), args...))
		if args[0] == "list-sessions" {
			return []byte(strings.Join([]string{
				"$4", "native", "1700000000", "codex", "ai,codex", "Codex", "friendly",
			}, separator) + "\n"), nil
		}
		return nil, nil
	})
	sessions, err := ReadLegacyMetadata(context.Background(), runner)
	if err != nil {
		t.Fatal(err)
	}
	if len(sessions) != 1 || sessions[0].Alias != "friendly" ||
		sessions[0].Profile != "codex" {
		t.Fatalf("legacy metadata=%#v", sessions)
	}
	if err := ClearLegacyMetadata(context.Background(), runner, sessions); err != nil {
		t.Fatal(err)
	}
	want := [][]string{
		{"list-sessions", "-F", strings.Join([]string{
			"#{session_id}", "#{session_name}", "#{session_created}",
			"#{@hmux_profile}", "#{@hmux_tags}", "#{@hmux_label}", "#{@hmux_alias}",
		}, separator)},
		{"set-option", "-u", "-t", "$4", "@hmux_profile"},
		{"set-option", "-u", "-t", "$4", "@hmux_tags"},
		{"set-option", "-u", "-t", "$4", "@hmux_label"},
		{"set-option", "-u", "-t", "$4", "@hmux_alias"},
	}
	if !reflect.DeepEqual(calls, want) {
		t.Fatalf("calls=%v want=%v", calls, want)
	}
}

func TestReadHandlesFiveHundredSessionsInTwoCalls(t *testing.T) {
	runner := &fakeLargeRunner{count: 500}
	value, err := Read(context.Background(), runner)
	if err != nil {
		t.Fatal(err)
	}
	if runner.calls != 2 || len(value.Sessions) != 500 {
		t.Fatalf("calls=%d sessions=%d", runner.calls, len(value.Sessions))
	}
	if value.Sessions[0].ActivityAt < value.Sessions[len(value.Sessions)-1].ActivityAt {
		t.Fatal("sessions are not sorted by recent activity")
	}
}

type fakeLargeRunner struct {
	count int
	calls int
}

func (f *fakeLargeRunner) Output(_ context.Context, args ...string) ([]byte, error) {
	f.calls++
	var out strings.Builder
	for index := 1; index <= f.count; index++ {
		switch args[0] {
		case "list-sessions":
			fmt.Fprintln(&out, strings.Join([]string{
				fmt.Sprintf("$%d", index), fmt.Sprintf("세션 %03d", index),
				fmt.Sprint(1700000000 + index), fmt.Sprint(1700001000 + index),
				"0", "1", "", "",
			}, separator))
		case "list-windows":
			fmt.Fprintln(&out, strings.Join([]string{
				fmt.Sprintf("$%d", index), "editor", "1",
				fmt.Sprintf("/work/아주-긴-프로젝트-%03d", index), "codex", "120", "40", "",
			}, separator))
		default:
			return nil, errors.New("unexpected command")
		}
	}
	return []byte(out.String()), nil
}

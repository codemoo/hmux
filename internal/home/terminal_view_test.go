package home

import (
	"context"
	"errors"
	"reflect"
	"testing"

	"github.com/codemoo/hmux/internal/catalog"
)

type terminalViewRunnerFunc func(context.Context, ...string) ([]byte, error)

func (f terminalViewRunnerFunc) Output(ctx context.Context, args ...string) ([]byte, error) {
	return f(ctx, args...)
}

func TestExpectedTerminalViewChecksIdentityAroundGroupedCreation(t *testing.T) {
	var calls [][]string
	runner := terminalViewRunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
		calls = append(calls, append([]string(nil), args...))
		if args[0] == "display-message" {
			return []byte("1700000000\n"), nil
		}
		return nil, nil
	})
	if err := createExpectedTerminalView(context.Background(), runner, "$8", 1700000000, "hmux-app-view-42-acde"); err != nil {
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

func TestExpectedTerminalViewCleansOnlyTemporaryViewAfterIdentityChange(t *testing.T) {
	displays := 0
	var calls [][]string
	runner := terminalViewRunnerFunc(func(_ context.Context, args ...string) ([]byte, error) {
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
	err := createExpectedTerminalView(context.Background(), runner, "$8", 1700000000, "hmux-app-view-42-acde")
	if !errors.Is(err, catalog.ErrSessionChanged) {
		t.Fatalf("err=%v", err)
	}
	last := calls[len(calls)-1]
	if !reflect.DeepEqual(last, []string{"kill-session", "-t", "hmux-app-view-42-acde"}) {
		t.Fatalf("cleanup=%v", last)
	}
}

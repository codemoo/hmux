package safeexec

import (
	"errors"
	"os/exec"
	"strings"
	"testing"
)

func TestOutputLimit(t *testing.T) {
	output, err := Output(exec.Command("printf", "12345"), 5)
	if err != nil || string(output) != "12345" {
		t.Fatalf("bounded output failed: output=%q err=%v", output, err)
	}
	if _, err := Output(exec.Command("printf", "123456"), 5); err == nil ||
		!strings.Contains(err.Error(), "exceeds 5 bytes") {
		t.Fatalf("oversized output was accepted: %v", err)
	}
}

func TestOutputKeepsBoundedStderrOutOfErrorText(t *testing.T) {
	_, err := Output(exec.Command("/bin/sh", "-c", "printf 'diagnostic' >&2; exit 7"), 16)
	if err == nil {
		t.Fatal("failed command was accepted")
	}
	if strings.Contains(err.Error(), "diagnostic") || Stderr(err) != "diagnostic" {
		t.Fatalf("stderr handling err=%q stderr=%q", err, Stderr(err))
	}
	var exit *exec.ExitError
	if !errors.As(err, &exit) || exit.ExitCode() != 7 {
		t.Fatalf("wrapped exit status was lost: %v", err)
	}
}

func TestPartialExitPreservesOnlyExplicitBoundedResult(t *testing.T) {
	for _, tc := range []struct {
		script   string
		limit    int64
		accepted bool
	}{
		{"printf record; exit 1", 32, true},
		{"printf record; exit 2", 32, false},
		{"printf oversized; exit 1", 2, false},
	} {
		raw, err := OutputOnPartialExit(exec.Command("/bin/sh", "-c", tc.script), tc.limit, 1)
		if tc.accepted {
			if err != nil || string(raw) != "record" {
				t.Fatal("bounded partial result lost")
			}
		} else if err == nil || len(raw) != 0 {
			t.Fatal("invalid partial result accepted")
		}
	}
	raw, err := Output(exec.Command("/bin/sh", "-c", "printf record; exit 1"), 32)
	if err == nil || len(raw) != 0 {
		t.Fatal("default execution no longer fails closed")
	}
}

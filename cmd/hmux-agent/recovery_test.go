package main

import (
	"io"
	"testing"
)

func TestRecoveryCLIRejectsUnallowlistedCommandsBeforeConfig(t *testing.T) {
	for _, args := range [][]string{nil, {"kill"}, {"save", "--path", "/tmp/untrusted"}, {"restore", "--force"}} {
		if err := runAgentRecovery(t.Context(), args, io.Discard); err == nil {
			t.Fatal("invalid recovery operation accepted")
		}
	}
}

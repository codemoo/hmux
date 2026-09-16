package main

import (
	"bytes"
	"context"
	"testing"
)

func TestConversationRejectsArbitraryPathsAndInvalidIdentities(t *testing.T) {
	for _, args := range [][]string{
		nil, {"--path", "/private/file"}, {"--session", "$1", "--created-at", "0"},
		{"--session", "$1;whoami", "--created-at", "12"}, {"--session", "$1", "--created-at", "12", "extra"},
	} {
		var output bytes.Buffer
		if runAgentConversation(context.Background(), args, &output) == nil || output.Len() != 0 {
			t.Fatal("invalid request was accepted")
		}
	}
}

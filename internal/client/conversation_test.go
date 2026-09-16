package client

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/codemoo/hmux/internal/config"
)

func TestConversationRemoteIdentityCapabilityAndPayload(t *testing.T) {
	for _, tc := range []struct {
		name, capability, payload string
		ok                        bool
	}{
		{"ready", "conversation-v1", `{"session_id":"$1","created_at":12,"status":"ready","messages":[{"id":"one","role":"assistant","text":"Readable answer"}],"truncated":false}`, true},
		{"old agent", "catalog-stream-v1", `{}`, false},
		{"wrong session", "conversation-v1", `{"session_id":"$2","created_at":12,"status":"ready","messages":[]}`, false},
		{"recycled session", "conversation-v1", `{"session_id":"$1","created_at":13,"status":"ready","messages":[]}`, false},
		{"reasoning forbidden", "conversation-v1", `{"session_id":"$1","created_at":12,"status":"ready","messages":[{"id":"one","role":"analysis","text":"hidden"}]}`, false},
		{"duplicate IDs", "conversation-v1", `{"session_id":"$1","created_at":12,"status":"ready","messages":[{"id":"same","role":"assistant","text":"A"},{"id":"same","role":"assistant","text":"B"}]}`, false},
		{"unavailable body", "conversation-v1", `{"session_id":"$1","created_at":12,"status":"unavailable","messages":[{"id":"one","role":"assistant","text":"not allowed"}]}`, false},
		{"extra field", "conversation-v1", `{"session_id":"$1","created_at":12,"status":"ready","messages":[],"path":"secret"}`, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			argsFile := filepath.Join(dir, "args")
			body := filepath.Join(dir, "body")
			if err := os.WriteFile(body, []byte(tc.payload), 0600); err != nil {
				t.Fatal(err)
			}
			script := "#!/bin/sh\ncase \"$*\" in *capabilities*) printf '%s\\n' \"$HMUX_TEST_CAP\";exit 0;;esac\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_ARGS\"\ncat \"$HMUX_TEST_BODY\"\n"
			if err := os.WriteFile(filepath.Join(dir, "ssh"), []byte(script), 0700); err != nil {
				t.Fatal(err)
			}
			t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
			t.Setenv("HMUX_TEST_CAP", tc.capability)
			t.Setenv("HMUX_TEST_ARGS", argsFile)
			t.Setenv("HMUX_TEST_BODY", body)
			cfg := config.DefaultClientConfig()
			cfg.Role = "remote"
			_, err := Conversation(context.Background(), cfg, "$1", 12)
			if (err == nil) != tc.ok {
				t.Fatalf("success=%t expected=%t", err == nil, tc.ok)
			}
			args, _ := os.ReadFile(argsFile)
			if tc.capability != "conversation-v1" {
				if len(args) > 0 {
					t.Fatal("unsupported command invoked")
				}
				return
			}
			if !strings.Contains(string(args), "conversation\n--session\n"+remoteSessionArg("$1")+"\n--created-at\n12\n") {
				t.Fatal("identity arguments not preserved")
			}
		})
	}
}

package catalog

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

func TestClaudeConversationUsesBoundTranscriptAndRechecksProvider(t *testing.T) {
	for _, changed := range []bool{false, true} {
		t.Run(map[bool]string{false: "stable", true: "changed"}[changed], func(t *testing.T) {
			home := t.TempDir()
			root := filepath.Join(home, ".claude", "projects")
			if err := os.MkdirAll(root, 0700); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(root, "fixture.jsonl")
			if err := os.WriteFile(path, []byte("{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Public answer\"}]}}\n"), 0600); err != nil {
				t.Fatal(err)
			}
			calls := 0
			result, err := readConversation(context.Background(), "$3", 1700000000, conversationDependencies{
				runner: &conversationCatalogRunner{createdAt: 1700000000, panePID: 77}, home: home,
				binding: func(context.Context, int, string) (conversationBinding, sessionBindingStatus) {
					calls++
					provider := "claude"
					if changed && calls > 1 {
						provider = "codex"
					}
					return conversationBinding{provider: provider, providerPID: 88, filePID: 88, path: path, root: root, recordID: "fixture"}, sessionBindingReady
				},
			})
			if err != nil {
				t.Fatal(err)
			}
			if changed {
				if result.Status != "ambiguous" || len(result.Messages) != 0 {
					t.Fatal("changed provider leaked dialogue")
				}
				return
			}
			if calls != 2 || result.Status != "ready" || result.Provider != "claude" || len(result.Messages) != 1 || result.Messages[0].Text != "Public answer" {
				t.Fatal("bound Claude dialogue unavailable")
			}
		})
	}
}

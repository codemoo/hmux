package catalog

import (
	"context"
	"os"
	"testing"
	"time"
)

// Opt-in, read-only acceptance: never attaches to or mutates existing sessions,
// and reports only provider counts (no session names, IDs, paths or messages).
func TestReadOnlyLiveSessionBindings(t *testing.T) {
	if os.Getenv("HMUX_RUN_SESSION_BINDING_READ_TEST") != "1" {
		t.Skip("read-only live session binding acceptance is opt-in")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	catalog, err := ReadBasic(ctx, TmuxRunner{})
	if err != nil {
		t.Fatal("tmux catalog unavailable")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal("Home unavailable")
	}
	inspector := systemProcessInspector{}
	nodes, err := inspector.processSnapshot(ctx)
	if err != nil {
		t.Fatal("process snapshot unavailable")
	}
	var panes []int
	seen := map[int]bool{}
	for _, session := range catalog.Sessions {
		if session.PanePID > 0 && !seen[session.PanePID] {
			seen[session.PanePID] = true
			panes = append(panes, session.PanePID)
		}
	}
	bindings := inspector.resolveSessionBindings(ctx, nodes, panes, home)
	ready, unknown := map[string]int{}, map[string]int{}
	for _, binding := range bindings {
		if binding.provider == "" {
			continue
		}
		if binding.status == sessionBindingReady {
			ready[binding.provider]++
		} else {
			unknown[binding.provider]++
		}
	}
	t.Logf("active tmux panes=%d; Codex ready=%d unavailable=%d; Claude ready=%d unavailable=%d", len(panes), ready["codex"], unknown["codex"], ready["claude"], unknown["claude"])
	refs, err := ResolveResumeReferences(ctx, panes)
	if err != nil {
		t.Fatal("live recovery reference check unavailable")
	}
	t.Logf("stable Home-only resume references=%d", len(refs))
	if ctx.Err() != nil {
		t.Fatal("binding deadline exceeded")
	}
}

func TestReadOnlyLiveClaudeConversation(t *testing.T) {
	if os.Getenv("HMUX_RUN_CLAUDE_CONVERSATION_READ_TEST") != "1" {
		t.Skip("read-only Claude acceptance is opt-in")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	cat, err := ReadBasic(ctx, TmuxRunner{})
	if err != nil {
		t.Fatal("catalog unavailable")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal("Home unavailable")
	}
	inspector := systemProcessInspector{}
	nodes, err := inspector.processSnapshot(ctx)
	if err != nil {
		t.Fatal("process snapshot unavailable")
	}
	var panes []int
	for _, s := range cat.Sessions {
		panes = append(panes, s.PanePID)
	}
	bindings := inspector.resolveSessionBindings(ctx, nodes, panes, home)
	count := 0
	for _, s := range cat.Sessions {
		if bindings[s.PanePID].provider != "claude" {
			continue
		}
		result, err := ReadConversation(ctx, s.ID, s.CreatedAt)
		if err != nil || result.Status != "ready" || result.Provider != "claude" {
			t.Fatalf("active Claude conversation unavailable: status=%s", result.Status)
		}
		users, answers := 0, 0
		for _, m := range result.Messages {
			if m.Role == "user" {
				users++
			}
			if m.Role == "assistant" {
				answers++
			}
		}
		if users == 0 || answers == 0 {
			t.Fatalf("expected public dialogue; users=%d answers=%d", users, answers)
		}
		count++
		t.Logf("Claude conversation ready: user messages=%d assistant messages=%d truncated=%v", users, answers, result.Truncated)
	}
	if count == 0 {
		t.Fatal("no active Claude sessions found")
	}
}

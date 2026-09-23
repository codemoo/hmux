package catalog

import (
	"context"
	"os"
	"testing"
	"time"
)

// Explicit opt-in: metadata only; never prints paths, identities or transcripts.
func TestReadOnlyLoadingTimings(t *testing.T) {
	if os.Getenv("HMUX_RUN_LOADING_READ_TEST") != "1" {
		t.Skip("live metadata timing is opt-in")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	start := time.Now()
	c, err := ReadBasic(ctx, TmuxRunner{})
	if err != nil {
		t.Fatal("basic catalog unavailable")
	}
	t.Logf("basic_catalog_ms=%d sessions=%d", time.Since(start).Milliseconds(), len(c.Sessions))
	s := systemProcessInspector{}
	start = time.Now()
	nodes, err := s.processSnapshot(ctx)
	if err != nil {
		t.Fatal("process snapshot unavailable")
	}
	t.Logf("process_snapshot_ms=%d", time.Since(start).Milliseconds())
	home, _ := os.UserHomeDir()
	var panes []int
	for _, v := range c.Sessions {
		panes = append(panes, v.PanePID)
	}
	start = time.Now()
	bindings := s.resolveSessionBindings(ctx, nodes, panes, home)
	t.Logf("provider_binding_and_state_ms=%d", time.Since(start).Milliseconds())
	count := 0
	start = time.Now()
	for _, b := range bindings {
		if b.provider == "codex" && b.status == sessionBindingReady {
			readCodexEvents(b.path, b.root)
			count++
		}
	}
	t.Logf("codex_record_scan_ms=%d records=%d", time.Since(start).Milliseconds(), count)
	start = time.Now()
	_, err = Read(ctx, TmuxRunner{})
	if err != nil {
		t.Fatal("full catalog unavailable")
	}
	t.Logf("full_catalog_ms=%d", time.Since(start).Milliseconds())
	if ctx.Err() != nil {
		t.Fatal("timing budget exceeded")
	}
}

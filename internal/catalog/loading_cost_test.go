package catalog

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestSyntheticStateScanCost(t *testing.T) {
	if os.Getenv("HMUX_RUN_LOADING_COST_TEST") != "1" {
		t.Skip("manual timing probe")
	}
	root := t.TempDir()
	path := filepath.Join(root, "fixture.jsonl")
	line := `{"type":"event_msg","payload":{"type":"agent_message","message":"` + strings.Repeat("x", 4096) + `"}}` + "\n"
	raw := strings.Repeat(line, int(eventTailLimit)/len(line))
	if err := os.WriteFile(path, []byte(raw), 0600); err != nil {
		t.Fatal(err)
	}
	start := time.Now()
	for i := 0; i < 25; i++ {
		readCodexEvents(path, root)
	}
	t.Logf("synthetic_25_records_ms=%d bytes_per_record=%d", time.Since(start).Milliseconds(), len(raw))
}

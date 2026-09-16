package wire

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestSharedSnapshotWireFixture(t *testing.T) {
	path := filepath.Join("..", "..", "Tests", "TokenUsageCoreTests", "Fixtures", "wire_snapshot_contract.json")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var snapshot UsageSnapshot
	if err := json.Unmarshal(data, &snapshot); err != nil {
		t.Fatal(err)
	}
	if snapshot.Provider != ProviderCodex || snapshot.Seq != 7 || snapshot.Status.QuotaSource != QuotaSourceCodexLB {
		t.Fatalf("snapshot contract = %+v", snapshot)
	}
	if got := strings.Join(snapshot.Status.ActivitySources, ","); got != "hermes,jsonl" {
		t.Fatalf("activity sources = %q", got)
	}
}

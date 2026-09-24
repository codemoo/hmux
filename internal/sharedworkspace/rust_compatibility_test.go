package sharedworkspace

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

// Actual Go merge and disk owner serve as the oracle; all identities and state
// are synthetic. No tmux or production workspace is consulted.
func TestRustWorkspaceOracle(t *testing.T) {
	type mergeCase struct {
		Name    string    `json:"name"`
		Current Snapshot  `json:"current"`
		Change  Change    `json:"change"`
		Result  *Snapshot `json:"result"`
	}
	var merges []mergeCase
	for _, c := range []struct {
		name                string
		current, base, next []model.SessionIdentity
	}{
		{"insert-before", ids(a), ids(a), ids(b, a)},
		{"concurrent-open", ids(a, c), ids(a), ids(a, b)},
		{"close-preserves-remote", ids(a, c), ids(a), ids()},
		{"stale-no-resurrection", ids(c), ids(a, c), ids(a, c)},
		{"stale-reorder", ids(c), ids(a, c), ids(c, a)},
		{"remote-order", ids(c, a, b), ids(a, b, c), ids(a, b, c)},
		{"explicit-reorder", ids(a, b, c), ids(a, b), ids(b, a)},
		{"close-all", ids(a, b), ids(a, b), ids()},
		{"recycled-lifetime", ids(a, c), ids(), ids(model.SessionIdentity{ID: a.ID, CreatedAt: 201})},
		{"duplicate-id", ids(a), ids(a), ids(a, model.SessionIdentity{ID: a.ID, CreatedAt: 201})},
	} {
		current := Snapshot{Version: 1, Tabs: c.current}
		ch := change(c.base, c.next)
		result, err := Merge(current, ch)
		var expected *Snapshot
		if err == nil {
			expected = &result
		}
		merges = append(merges, mergeCase{c.name, current, ch, expected})
	}
	type syncCase struct {
		Name     string          `json:"name"`
		Change   *Change         `json:"change"`
		Sessions []model.Session `json:"sessions"`
		Result   Snapshot        `json:"result"`
	}
	store := Store{StateDir: t.TempDir()}
	var syncs []syncCase
	apply := func(name string, ch *Change, sessions []model.Session) Snapshot {
		t.Helper()
		result, err := store.Sync(context.Background(), ch, func(context.Context) (model.Catalog, error) { return model.Catalog{Sessions: sessions}, nil })
		if err != nil {
			t.Fatal(err)
		}
		syncs = append(syncs, syncCase{name, ch, sessions, result})
		return result
	}
	cat, _ := fixtureCatalog(context.Background())
	apply("empty-poll", nil, cat.Sessions)
	first := change(ids(), ids(a))
	first.Selected = &a
	initial := apply("first-open", &first, cat.Sessions)
	apply("poll-preserves-revision", nil, cat.Sessions)
	second := change(ids(), ids(b))
	second.OperationID = "operation-00000002"
	after := apply("concurrent-open", &second, cat.Sessions)
	apply("replay", &first, cat.Sessions)
	missing := change(ids(a, b), ids(a, b, c))
	missing.OperationID = "operation-missing1"
	missing.Revision = after.Revision
	apply("missing-new-tab", &missing, nil)
	apply("missing-existing-tabs-survive", nil, nil)
	future := change(ids(), ids())
	future.OperationID = "operation-future1"
	future.Revision = 1000
	apply("future-revision", &future, cat.Sessions)
	restored := []model.Session{{ID: "$9", CreatedAt: 201, RestoredFrom: &a}, {ID: b.ID, CreatedAt: b.CreatedAt}}
	after = apply("restored-identity", nil, restored)
	close := change(ids(a), ids())
	close.OperationID = "operation-close01"
	close.Revision = initial.Revision
	after = apply("old-identity-close", &close, restored)
	for i := 0; i < 65; i++ {
		ch := change(ids(), ids())
		ch.OperationID = fmt.Sprintf("operation-history-%03d", i)
		ch.Revision = after.Revision
		after = apply("history-window", &ch, restored)
	}
	apply("expired-retry", &first, restored)
	raw, err := json.MarshalIndent(map[string]any{"merges": merges, "syncs": syncs}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join("..", "..", "tests", "fixtures", "workspace-v1", "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_WORKSPACE_FIXTURE") == "1" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, raw, 0644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(raw, want) {
		t.Fatal("Go workspace oracle changed")
	}
}

func TestRustWorkspaceHandoff(t *testing.T) {
	root := os.Getenv("HMUX_RUST_WORKSPACE_HANDOFF")
	if root == "" {
		t.Skip("isolated directory supplied by make rust-compat")
	}
	store := Store{StateDir: root}
	got, err := store.Sync(context.Background(), nil, fixtureCatalog)
	if err != nil || got.Revision != 1 || len(got.Tabs) != 1 || got.Tabs[0] != a {
		t.Fatal("Rust state not readable", got, err)
	}
	next := change(ids(), ids(b))
	next.OperationID = "operation-go-000002"
	next.Revision = got.Revision
	saved, err := store.Sync(context.Background(), &next, fixtureCatalog)
	if err != nil || saved.Revision != 2 || len(saved.Tabs) != 2 {
		t.Fatal("Go current-state update failed", saved, err)
	}
}

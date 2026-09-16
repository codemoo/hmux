package recovery

import (
	"context"
	"testing"

	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

func TestSharedTabsComposeRecoveryWithoutClientSync(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	a := model.SessionIdentity{ID: "$1", CreatedAt: 100}
	b := model.SessionIdentity{ID: "$2", CreatedAt: 200}
	c := model.SessionIdentity{ID: "$3", CreatedAt: 300}
	store := sharedworkspace.Store{StateDir: dir}
	init := sharedworkspace.Change{OperationID: "initialize-tabs-001", Tabs: []model.SessionIdentity{a}}
	if _, err := store.Sync(ctx, &init, func(context.Context) (model.Catalog, error) {
		return model.Catalog{Sessions: []model.Session{{ID: a.ID, CreatedAt: a.CreatedAt}}}, nil
	}); err != nil {
		t.Fatal(err)
	}
	first := []restoredIdentity{{From: a, To: b, Name: "work"}}
	second := []restoredIdentity{{From: b, To: c, Name: "work"}}
	live := snapshot{Sessions: []savedSession{{Identity: c, Name: "work"}}}
	// Deliberately skip any workspace read at B, as can happen during rollout or
	// interrupted recovery. The checkpoint composes A->B and B->C exactly.
	if err := (Store{StateDir: dir}).rebaseSharedWorkspace(ctx, first, second, live); err != nil {
		t.Fatal(err)
	}
	current, err := store.Sync(ctx, nil, func(context.Context) (model.Catalog, error) {
		return model.Catalog{Sessions: []model.Session{{ID: c.ID, CreatedAt: c.CreatedAt}}}, nil
	})
	if err != nil || len(current.Tabs) != 1 || current.Tabs[0] != c {
		t.Fatalf("shared tab failed multi-boot recovery: %v %v", current, err)
	}
}

package sharedworkspace

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"sync"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

var a = model.SessionIdentity{ID: "$1", CreatedAt: 101}
var b = model.SessionIdentity{ID: "$2", CreatedAt: 102}
var c = model.SessionIdentity{ID: "$3", CreatedAt: 103}

func change(base, tabs []model.SessionIdentity) Change {
	return Change{OperationID: "operation-00000001", Base: base, Tabs: tabs}
}
func ids(values ...model.SessionIdentity) []model.SessionIdentity {
	return append([]model.SessionIdentity{}, values...)
}
func TestMergeConcurrentDeltas(t *testing.T) {
	cases := []struct {
		name                      string
		current, base, next, want []model.SessionIdentity
	}{
		{"new tab requested before existing", ids(a), ids(a), ids(b, a), ids(b, a)},
		{"concurrent open", ids(a, c), ids(a), ids(a, b), ids(a, b, c)},
		{"close preserves remote open", ids(a, c), ids(a), ids(), ids(c)},
		{"stale selection cannot resurrect remote close", ids(c), ids(a, c), ids(a, c), ids(c)},
		{"stale reorder cannot resurrect remote close", ids(c), ids(a, c), ids(c, a), ids(c)},
		{"untouched order preserves remote reorder", ids(c, a, b), ids(a, b, c), ids(a, b, c), ids(c, a, b)},
		{"explicit reorder preserves unseen tab", ids(a, b, c), ids(a, b), ids(b, a), ids(b, a, c)},
		{"explicit empty", ids(a, b), ids(a, b), ids(), ids()},
		{"recycled identity replaces old lifetime", ids(a), ids(a), ids(a, model.SessionIdentity{ID: "$1", CreatedAt: 201}), nil},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := Merge(Snapshot{Version: 1, Tabs: tc.current}, change(tc.base, tc.next))
			if tc.want == nil {
				if err == nil {
					t.Fatal("invalid duplicate IDs accepted")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if !reflect.DeepEqual(got.Tabs, tc.want) {
				t.Fatalf("got %v want %v", got.Tabs, tc.want)
			}
		})
	}
	newer := model.SessionIdentity{ID: a.ID, CreatedAt: a.CreatedAt + 100}
	got, err := Merge(Snapshot{Version: 1, Tabs: ids(a, c)}, change(ids(), ids(newer)))
	if err != nil || !reflect.DeepEqual(got.Tabs, ids(c, newer)) {
		t.Fatalf("recycled ID: %v %v", got, err)
	}
}
func fixtureCatalog(_ context.Context) (model.Catalog, error) {
	return model.Catalog{Sessions: []model.Session{{ID: a.ID, CreatedAt: a.CreatedAt}, {ID: b.ID, CreatedAt: b.CreatedAt}, {ID: c.ID, CreatedAt: c.CreatedAt}}}, nil
}
func TestStoreIdempotencyAndExpiredRetry(t *testing.T) {
	s := Store{StateDir: t.TempDir()}
	ctx := context.Background()
	first := change(ids(), ids(a))
	got, err := s.Sync(ctx, &first, fixtureCatalog)
	if err != nil {
		t.Fatal(err)
	}
	close := change(ids(a), ids())
	close.OperationID = "operation-00000002"
	close.Revision = got.Revision
	after, err := s.Sync(ctx, &close, fixtureCatalog)
	if err != nil {
		t.Fatal(err)
	}
	replay, err := s.Sync(ctx, &first, fixtureCatalog)
	if err != nil || replay.Revision != after.Revision || len(replay.Tabs) != 0 {
		t.Fatalf("retry resurrected close: %v %v", replay, err)
	}
	for i := 3; i < 70; i++ {
		ch := change(ids(), ids())
		ch.OperationID = fmt.Sprintf("operation-%08d", i)
		ch.Revision = after.Revision
		after, err = s.Sync(ctx, &ch, fixtureCatalog)
		if err != nil {
			t.Fatal(err)
		}
	}
	if v, err := s.Sync(ctx, &first, fixtureCatalog); err != nil || v.Conflict != "workspace_conflict" {
		t.Fatal("expired operation accepted")
	}
	future := change(ids(), ids())
	future.Revision = after.Revision + 1
	future.OperationID = "operation-future00"
	if v, err := s.Sync(ctx, &future, fixtureCatalog); err != nil || v.Conflict != "workspace_conflict" {
		t.Fatal("future revision accepted")
	}
}
func TestStoreConcurrentWriters(t *testing.T) {
	s := Store{StateDir: t.TempDir()}
	ctx := context.Background()
	var wg sync.WaitGroup
	for i, id := range ids(a, b, c) {
		wg.Add(1)
		go func(i int, id model.SessionIdentity) {
			defer wg.Done()
			ch := change(ids(), ids(id))
			ch.OperationID = fmt.Sprintf("operation-%08d", i)
			if _, err := s.Sync(ctx, &ch, fixtureCatalog); err != nil {
				t.Error(err)
			}
		}(i, id)
	}
	wg.Wait()
	got, err := s.Sync(ctx, nil, fixtureCatalog)
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Tabs) != 3 {
		t.Fatalf("lost concurrent tab: %v", got.Tabs)
	}
	for _, id := range ids(a, b, c) {
		if !contains(got.Tabs, id) {
			t.Fatal("missing tab", id)
		}
	}
}
func TestStoreRecoveryAndMissing(t *testing.T) {
	s := Store{StateDir: t.TempDir()}
	ctx := context.Background()
	ch := change(ids(), ids(a))
	ch.Selected = &a
	if _, err := s.Sync(ctx, &ch, fixtureCatalog); err != nil {
		t.Fatal(err)
	}
	missing := func(context.Context) (model.Catalog, error) { return model.Catalog{}, nil }
	got, err := s.Sync(ctx, nil, missing)
	if err != nil || !reflect.DeepEqual(got.Tabs, ids(a)) {
		t.Fatalf("missing tab erased: %v %v", got, err)
	}
	recovered := model.SessionIdentity{ID: "$9", CreatedAt: 201}
	catalog := func(context.Context) (model.Catalog, error) {
		return model.Catalog{Sessions: []model.Session{{ID: recovered.ID, CreatedAt: recovered.CreatedAt, RestoredFrom: &a}}}, nil
	}
	got, err = s.Sync(ctx, nil, catalog)
	if err != nil || !reflect.DeepEqual(got.Tabs, ids(recovered)) || got.Selected != nil {
		t.Fatalf("lineage not rebased: %v %v", got, err)
	}
	close := change(ids(a), ids())
	close.OperationID = "operation-close001"
	close.Revision = got.Revision
	got, err = s.Sync(ctx, &close, catalog)
	if err != nil || len(got.Tabs) != 0 {
		t.Fatalf("old identity close: %v %v", got, err)
	}
	addMissing := change(ids(), ids(b))
	addMissing.OperationID = "operation-missing1"
	addMissing.Revision = got.Revision
	if v, err := s.Sync(ctx, &addMissing, catalog); err != nil || v.Conflict != "workspace_conflict" {
		t.Fatal("new missing tab accepted")
	}
}
func TestStoreRejectsUnsafeState(t *testing.T) {
	for _, target := range []string{"directory", "lock", "workspace.json"} {
		t.Run(target, func(t *testing.T) {
			root := t.TempDir()
			s := Store{StateDir: root}
			dir := filepath.Join(root, "shared-workspace")
			if target == "directory" {
				if err := os.Symlink(t.TempDir(), dir); err != nil {
					t.Fatal(err)
				}
			} else {
				if err := os.Mkdir(dir, 0700); err != nil {
					t.Fatal(err)
				}
				if err := os.Symlink(filepath.Join(root, "outside"), filepath.Join(dir, target)); err != nil {
					t.Fatal(err)
				}
			}
			if _, err := s.Sync(context.Background(), nil, fixtureCatalog); err == nil {
				t.Fatal("symlink accepted")
			}
		})
	}
	s := Store{StateDir: t.TempDir()}
	dir := filepath.Join(s.StateDir, "shared-workspace")
	if err := os.Mkdir(dir, 0755); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Sync(context.Background(), nil, fixtureCatalog); err == nil {
		t.Fatal("public directory accepted")
	}
}

func TestReadsDoNotCreateRevisions(t *testing.T) {
	s := Store{StateDir: t.TempDir()}
	ctx := context.Background()
	first, err := s.Sync(ctx, nil, fixtureCatalog)
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 3; i++ {
		next, err := s.Sync(ctx, nil, fixtureCatalog)
		if err != nil || next.Revision != first.Revision {
			t.Fatalf("poll created revision: %v %v", next, err)
		}
	}
	ch := change(ids(), ids(a))
	saved, err := s.Sync(ctx, &ch, fixtureCatalog)
	if err != nil {
		t.Fatal(err)
	}
	next, err := s.Sync(ctx, nil, fixtureCatalog)
	if err != nil || saved.Revision != next.Revision {
		t.Fatalf("initialized poll created revision: %v %v", next, err)
	}
}

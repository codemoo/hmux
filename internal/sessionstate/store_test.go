package sessionstate

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

func TestStoreAppliesMetadataOnlyToMatchingSessionIdentity(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	session := model.Session{ID: "$7", Name: "native-name", CreatedAt: 1700000000}
	profile := model.Profile{
		ID: "codex", Label: "Codex", Tags: []string{"ai", "codex"},
	}
	if err := store.SetProfile(session, profile); err != nil {
		t.Fatal(err)
	}
	if err := store.SetAlias(session, "friendly"); err != nil {
		t.Fatal(err)
	}
	value := model.Catalog{Sessions: []model.Session{
		session,
		{ID: "$7", Name: "reused", CreatedAt: 1800000000},
	}}
	if err := store.Apply(&value); err != nil {
		t.Fatal(err)
	}
	if value.Sessions[0].Alias != "friendly" ||
		value.Sessions[0].Profile != "codex" ||
		value.Sessions[0].Label != "Codex" ||
		len(value.Sessions[0].Tags) != 2 {
		t.Fatalf("metadata was not applied: %#v", value.Sessions[0])
	}
	if value.Sessions[1].Alias != "" || value.Sessions[1].Profile != "" {
		t.Fatalf("metadata leaked to a reused tmux ID: %#v", value.Sessions[1])
	}
}

func TestStoreRejectsUnsafeAliasAndSymlinkState(t *testing.T) {
	root := t.TempDir()
	store := Store{StateDir: root}
	session := model.Session{ID: "$1", Name: "one", CreatedAt: 1700000000}
	if err := store.SetAlias(session, "bad\nalias"); err == nil {
		t.Fatal("control character in alias was accepted")
	}
	if err := store.SetAlias(session, "safe"); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "sessions", "sessions.json")
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(filepath.Join(t.TempDir(), "outside"), path); err != nil {
		t.Fatal(err)
	}
	if err := store.SetAlias(session, "other"); err == nil {
		t.Fatal("symlinked session metadata target was accepted")
	}
}

func TestImportPreservesLegacyAliasAndProfileOutsideTmux(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	legacy := model.Session{
		ID: "$9", Name: "legacy", CreatedAt: 1700000000,
		Alias: "display", Profile: "claude", Label: "Claude",
		Tags: []string{"ai", "claude"},
	}
	if err := store.Import([]model.Session{legacy}); err != nil {
		t.Fatal(err)
	}
	value := model.Catalog{Sessions: []model.Session{{
		ID: "$9", Name: "legacy", CreatedAt: 1700000000,
	}}}
	if err := store.Apply(&value); err != nil {
		t.Fatal(err)
	}
	if value.Sessions[0].Alias != "display" || value.Sessions[0].Profile != "claude" {
		t.Fatalf("legacy metadata was not imported: %#v", value.Sessions[0])
	}
}

func TestVisibilityIsReversibleAndBoundToSessionLifetime(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	session := model.Session{ID: "$12", Name: "native", CreatedAt: 1700000000}
	if err := store.SetHidden(session, true); err != nil {
		t.Fatal(err)
	}
	value := model.Catalog{Sessions: []model.Session{
		session,
		{ID: "$12", Name: "reused", CreatedAt: 1800000000},
	}}
	if err := store.ApplyVisibility(&value); err != nil {
		t.Fatal(err)
	}
	if !value.Sessions[0].Hidden || value.Sessions[1].Hidden {
		t.Fatalf("visibility leaked across identity: %#v", value.Sessions)
	}
	if err := store.SetHidden(session, false); err != nil {
		t.Fatal(err)
	}
	value.Sessions[0].Hidden = true
	if err := store.ApplyVisibility(&value); err != nil {
		t.Fatal(err)
	}
	if value.Sessions[0].Hidden {
		t.Fatal("restored session remained hidden")
	}
}

func TestVisibilityRejectsSymlinkAndUnsafeMode(t *testing.T) {
	root := t.TempDir()
	store := Store{StateDir: root}
	session := model.Session{ID: "$4", Name: "safe", CreatedAt: 1700000000}
	if err := store.SetHidden(session, true); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "sessions", "session-visibility.json")
	if err := os.Chmod(path, 0o622); err != nil {
		t.Fatal(err)
	}
	if err := store.ApplyVisibility(&model.Catalog{}); err == nil {
		t.Fatal("group/world-writable visibility state was accepted")
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(filepath.Join(t.TempDir(), "outside"), path); err != nil {
		t.Fatal(err)
	}
	if err := store.SetHidden(session, false); err == nil {
		t.Fatal("symlinked visibility state was accepted")
	}
}

func TestSetHiddenExpectedRejectsStaleRestoreWithoutDeletingReplacement(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	oldSession := model.Session{ID: "$12", Name: "old", CreatedAt: 1700000000}
	newSession := model.Session{ID: "$12", Name: "new", CreatedAt: 1800000000}
	if err := store.SetHidden(newSession, true); err != nil {
		t.Fatal(err)
	}
	resolveNew := func(context.Context, string) (model.Session, error) { return newSession, nil }
	if err := store.SetHiddenExpected(context.Background(), oldSession.ID, oldSession.CreatedAt, false, resolveNew); !errors.Is(err, ErrSessionChanged) {
		t.Fatalf("stale restore error=%v", err)
	}
	value := model.Catalog{Sessions: []model.Session{newSession}}
	if err := store.ApplyVisibility(&value); err != nil {
		t.Fatal(err)
	}
	if !value.Sessions[0].Hidden {
		t.Fatal("stale restore deleted the replacement lifetime's hidden entry")
	}
}

func TestSetAliasExpectedRejectsStaleWriteWithoutClobberingReplacement(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	oldSession := model.Session{ID: "$7", Name: "old", CreatedAt: 1700000000}
	newSession := model.Session{ID: "$7", Name: "new", CreatedAt: 1800000000}
	if err := store.SetAlias(newSession, "replacement"); err != nil {
		t.Fatal(err)
	}
	resolveNew := func(context.Context, string) (model.Session, error) { return newSession, nil }
	if err := store.SetAliasExpected(context.Background(), oldSession.ID, oldSession.CreatedAt, "stale", resolveNew); !errors.Is(err, ErrSessionChanged) {
		t.Fatalf("stale alias error=%v", err)
	}
	value := model.Catalog{Sessions: []model.Session{newSession}}
	if err := store.Apply(&value); err != nil {
		t.Fatal(err)
	}
	if value.Sessions[0].Alias != "replacement" {
		t.Fatalf("replacement alias was clobbered: %#v", value.Sessions[0])
	}
}

func TestExpectedMutationUsesLiveNameResolvedUnderLock(t *testing.T) {
	store := Store{StateDir: t.TempDir()}
	live := model.Session{ID: "$8", Name: "renamed", CreatedAt: 1700000000}
	resolveLive := func(context.Context, string) (model.Session, error) { return live, nil }
	if err := store.SetAliasExpected(context.Background(), live.ID, live.CreatedAt, "friendly", resolveLive); err != nil {
		t.Fatal(err)
	}
	value := model.Catalog{Sessions: []model.Session{live}}
	if err := store.Apply(&value); err != nil {
		t.Fatal(err)
	}
	if value.Sessions[0].Alias != "friendly" {
		t.Fatalf("alias did not bind to the live name: %#v", value.Sessions[0])
	}
}

func TestStoresShareDisplayNameAndVisibilityAcrossHMuxClients(t *testing.T) {
	root := t.TempDir()
	firstClient := Store{StateDir: root}
	secondClient := Store{StateDir: root}
	session := model.Session{ID: "$21", Name: "native", CreatedAt: 1700000000}

	if err := firstClient.SetAlias(session, "shared display name"); err != nil {
		t.Fatal(err)
	}
	if err := firstClient.SetHidden(session, true); err != nil {
		t.Fatal(err)
	}
	fromSecondClient := model.Catalog{Sessions: []model.Session{session}}
	if err := secondClient.Apply(&fromSecondClient); err != nil {
		t.Fatal(err)
	}
	if err := secondClient.ApplyVisibility(&fromSecondClient); err != nil {
		t.Fatal(err)
	}
	if fromSecondClient.Sessions[0].Alias != "shared display name" || !fromSecondClient.Sessions[0].Hidden {
		t.Fatalf("first client state was not visible to second client: %#v", fromSecondClient.Sessions[0])
	}

	if err := secondClient.SetAlias(session, "renamed elsewhere"); err != nil {
		t.Fatal(err)
	}
	if err := secondClient.SetHidden(session, false); err != nil {
		t.Fatal(err)
	}
	fromFirstClient := model.Catalog{Sessions: []model.Session{session}}
	if err := firstClient.Apply(&fromFirstClient); err != nil {
		t.Fatal(err)
	}
	if err := firstClient.ApplyVisibility(&fromFirstClient); err != nil {
		t.Fatal(err)
	}
	if fromFirstClient.Sessions[0].Alias != "renamed elsewhere" || fromFirstClient.Sessions[0].Hidden {
		t.Fatalf("second client state was not visible to first client: %#v", fromFirstClient.Sessions[0])
	}
}

func TestConcurrentHMuxClientsPreserveDistinctDisplayNameWrites(t *testing.T) {
	root := t.TempDir()
	firstClient := Store{StateDir: root}
	secondClient := Store{StateDir: root}
	firstSession := model.Session{ID: "$31", Name: "first", CreatedAt: 1700000001}
	secondSession := model.Session{ID: "$32", Name: "second", CreatedAt: 1700000002}
	start := make(chan struct{})
	errors := make(chan error, 2)
	go func() {
		<-start
		errors <- firstClient.SetAlias(firstSession, "first display")
	}()
	go func() {
		<-start
		errors <- secondClient.SetAlias(secondSession, "second display")
	}()
	close(start)
	for range 2 {
		if err := <-errors; err != nil {
			t.Fatal(err)
		}
	}

	value := model.Catalog{Sessions: []model.Session{firstSession, secondSession}}
	if err := (Store{StateDir: root}).Apply(&value); err != nil {
		t.Fatal(err)
	}
	if value.Sessions[0].Alias != "first display" || value.Sessions[1].Alias != "second display" {
		t.Fatalf("a concurrent display-name write was lost: %#v", value.Sessions)
	}
}

func TestVisibilityRejectsPeerReadableModes(t *testing.T) {
	for _, mode := range []os.FileMode{0o644, 0o640, 0o604} {
		root := t.TempDir()
		store := Store{StateDir: root}
		session := model.Session{ID: "$4", Name: "safe", CreatedAt: 1700000000}
		if err := store.SetHidden(session, true); err != nil {
			t.Fatal(err)
		}
		path := filepath.Join(root, "sessions", "session-visibility.json")
		if err := os.Chmod(path, mode); err != nil {
			t.Fatal(err)
		}
		if err := store.ApplyVisibility(&model.Catalog{}); err == nil {
			t.Fatalf("peer-readable mode %o was accepted", mode)
		}
	}
}

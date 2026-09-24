package sessionstate

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"

	"github.com/codemoo/hmux/internal/model"
)

// TestRustSessionStateOracle is run only by the isolated Rust integration test.
// Every mode operates on its disposable hmux-e2e-* directory; no tmux is used.
func TestRustSessionStateOracle(t *testing.T) {
	if os.Getenv("HMUX_GO_SESSIONSTATE_ORACLE") != "1" {
		t.Skip("requires the isolated Rust session-state oracle")
	}
	root := os.Getenv("HMUX_GO_SESSIONSTATE_DIR")
	if !filepath.IsAbs(root) || !strings.HasPrefix(filepath.Base(root), "hmux-e2e-sessionstate-") {
		t.Fatal("oracle requires an absolute disposable hmux-e2e-sessionstate-* directory")
	}
	store := Store{StateDir: root}
	live := model.Session{ID: "$7", Name: "native", CreatedAt: 1700000000}
	sibling := model.Session{ID: "$8", Name: "sibling", CreatedAt: 1700000001}
	resolve := func(_ context.Context, id string) (model.Session, error) {
		if id != live.ID {
			return model.Session{}, ErrSessionChanged
		}
		return live, nil
	}
	switch os.Getenv("HMUX_GO_SESSIONSTATE_MODE") {
	case "seed":
		if err := store.SetProfile(live, model.Profile{ID: "codex", Label: "Codex", Tags: []string{"ai", "codex"}}); err != nil {
			t.Fatal(err)
		}
		if err := store.SetAliasExpected(context.Background(), live.ID, live.CreatedAt, "go-seed", resolve); err != nil {
			t.Fatal(err)
		}
		if err := store.SetHiddenExpected(context.Background(), live.ID, live.CreatedAt, true, resolve); err != nil {
			t.Fatal(err)
		}
		if err := store.SetAlias(sibling, "sibling-alias"); err != nil {
			t.Fatal(err)
		}
		fmt.Println("seeded")
	case "check-update":
		value := model.Catalog{Sessions: []model.Session{live, sibling}}
		if err := store.Apply(&value); err != nil {
			t.Fatal(err)
		}
		if err := store.ApplyVisibility(&value); err != nil {
			t.Fatal(err)
		}
		if got := value.Sessions[0]; got.Alias != "rust-alias" || got.Profile != "codex" || got.Label != "Codex" || len(got.Tags) != 2 || got.Hidden {
			t.Fatalf("Go could not read Rust's current state: %+v", got)
		}
		if got := value.Sessions[1]; got.Alias != "sibling-alias" {
			t.Fatalf("Rust clobbered another session's metadata: %+v", got)
		}
		if err := store.SetAliasExpected(context.Background(), live.ID, live.CreatedAt, "go-final", resolve); err != nil {
			t.Fatal(err)
		}
		if err := store.SetHiddenExpected(context.Background(), live.ID, live.CreatedAt, true, resolve); err != nil {
			t.Fatal(err)
		}
		staleTime := live.CreatedAt - 1
		if err := store.SetAliasExpected(context.Background(), live.ID, staleTime, "stale", resolve); !errors.Is(err, ErrSessionChanged) {
			t.Fatalf("stale alias write: %v", err)
		}
		if err := store.SetHiddenExpected(context.Background(), live.ID, staleTime, false, resolve); !errors.Is(err, ErrSessionChanged) {
			t.Fatalf("stale visibility restore: %v", err)
		}
		fmt.Println("checked")
	case "try-lock", "hold-lock":
		name := os.Getenv("HMUX_GO_SESSIONSTATE_LOCK")
		if name != "sessions.lock" && name != "visibility.lock" {
			t.Fatal("invalid oracle lock name")
		}
		path := filepath.Join(root, "sessions", name)
		file, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE, 0o600)
		if err != nil {
			t.Fatal(err)
		}
		defer file.Close()
		flags := syscall.LOCK_EX | syscall.LOCK_NB
		if os.Getenv("HMUX_GO_SESSIONSTATE_MODE") == "hold-lock" {
			flags = syscall.LOCK_EX
		}
		if err := syscall.Flock(int(file.Fd()), flags); err != nil {
			if errors.Is(err, syscall.EWOULDBLOCK) && flags&syscall.LOCK_NB != 0 {
				fmt.Println("busy")
				return
			}
			t.Fatal(err)
		}
		defer syscall.Flock(int(file.Fd()), syscall.LOCK_UN)
		fmt.Println("locked")
		if flags&syscall.LOCK_NB == 0 {
			if _, err := bufio.NewReader(os.Stdin).ReadBytes('\n'); err != nil {
				t.Fatal(err)
			}
		}
	default:
		t.Fatal("invalid oracle mode")
	}
}

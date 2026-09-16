package auth

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"github.com/codemoo/token-terrier/server-go/internal/wire"
)

func TestLocalSourceWriteIsAtomicDurableAndPrivate(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, ".credentials.json")
	source := &LocalSource{ClaudePath: path}
	body := []byte(`{"claudeAiOauth":{"accessToken":"new"}}`)
	if err := source.Write(context.Background(), wire.ProviderClaude, body); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != string(body) {
		t.Fatalf("body = %q, want %q", got, body)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if gotMode := info.Mode().Perm(); gotMode != 0o600 {
		t.Fatalf("credential mode = %o, want 600", gotMode)
	}
	lockInfo, err := os.Stat(filepath.Join(directory, ".credentials.lock"))
	if err != nil {
		t.Fatal(err)
	}
	if gotMode := lockInfo.Mode().Perm(); gotMode != 0o600 {
		t.Fatalf("lock mode = %o, want 600", gotMode)
	}
	leftovers, err := filepath.Glob(filepath.Join(directory, ".credentials.json.token-terrier-*"))
	if err != nil {
		t.Fatal(err)
	}
	if len(leftovers) != 0 {
		t.Fatalf("temporary files remain: %v", leftovers)
	}
}

func TestCredentialStoreDetectsAtomicCLISwitchByInode(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, "auth.json")
	first := []byte(`{"tokens":{"access_token":"token-aaaaaaaa","refresh_token":"refresh-1","account_id":"account-one"}}`)
	second := []byte(`{"tokens":{"access_token":"token-bbbbbbbb","refresh_token":"refresh-2","account_id":"account-two"}}`)
	if len(first) != len(second) {
		t.Fatal("fixture sizes must match so inode detection is exercised")
	}
	if err := os.WriteFile(path, first, 0o600); err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	store := NewCredentialStore(&LocalSource{CodexPath: path})
	if got := store.CurrentAccountKey(context.Background(), wire.ProviderCodex); got != "id:account-one" {
		t.Fatalf("initial account key = %q", got)
	}

	replacement := filepath.Join(directory, ".auth.json.cli-replacement")
	if err := os.WriteFile(replacement, second, 0o600); err != nil {
		t.Fatal(err)
	}
	// Preserve size and mtime to prove atomic-rename inode changes invalidate
	// the cache even on filesystems with coarse timestamp resolution.
	mtime := info.ModTime()
	if err := os.Chtimes(replacement, mtime, mtime); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(replacement, path); err != nil {
		t.Fatal(err)
	}
	if got := store.CurrentAccountKey(context.Background(), wire.ProviderCodex); got != "id:account-two" {
		t.Fatalf("account key after CLI replacement = %q, want account-two", got)
	}

	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Load(context.Background(), wire.ProviderCodex); !IsNotFound(err) {
		t.Fatalf("removed credential served from stale cache: %v", err)
	}
}

func TestLocalSourceConcurrentWritersNeverProduceMixedJSON(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, "auth.json")
	source := &LocalSource{CodexPath: path}
	const writers = 24
	var wg sync.WaitGroup
	errors := make(chan error, writers)
	for i := 0; i < writers; i++ {
		wg.Add(1)
		go func(value int) {
			defer wg.Done()
			body, _ := json.Marshal(map[string]int{"writer": value})
			if err := source.Write(context.Background(), wire.ProviderCodex, body); err != nil {
				errors <- err
			}
		}(i)
	}
	wg.Wait()
	close(errors)
	for err := range errors {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var payload map[string]int
	if err := json.Unmarshal(data, &payload); err != nil {
		t.Fatalf("final credential is torn: %v; body=%q", err, data)
	}
	if payload["writer"] < 0 || payload["writer"] >= writers {
		t.Fatalf("unexpected final payload: %v", payload)
	}
	if got := credentialLockPath(path); got != filepath.Join(directory, "auth.lock") {
		t.Fatalf("lock path = %q", got)
	}
}

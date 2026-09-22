package config

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestSetupHomeDefaultAndPreservedCustomDirectories(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(root, "config")
	t.Setenv("HOME", root)
	if err := SetupHome(dir, ""); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadHome(filepath.Join(dir, "home.toml"))
	if err != nil {
		t.Fatal(err)
	}
	inv, err := LoadInventory(cfg.InventoryPath)
	if err != nil {
		t.Fatal(err)
	}
	for _, profile := range inv.Profiles {
		if profile.DefaultDirectory != "~/.hmux" {
			t.Fatalf("wrong default %+v", profile)
		}
	}
	// Retired topology is still supported as data and must survive explicit updates.
	legacy := "\n[[clients]]\nid = \"old-home\"\nrole = \"home\"\nhostnames = [\"fixture.invalid\"]\n"
	file, err := os.OpenFile(cfg.InventoryPath, os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := file.WriteString(legacy); err != nil {
		t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	custom := filepath.Join(root, "custom work")
	if err := SetupHome(dir, custom); err != nil {
		t.Fatal(err)
	}
	backups, _ := filepath.Glob(cfg.InventoryPath + ".hmux-backup-*")
	if len(backups) != 1 {
		t.Fatalf("backups=%v", backups)
	}
	original, err := LoadInventory(backups[0])
	if err != nil || original.Profiles[0].DefaultDirectory != "~/.hmux" {
		t.Fatalf("backup=%+v err=%v", original, err)
	}
	before, _ := os.ReadFile(cfg.InventoryPath)
	if !strings.Contains(string(before), "fixture.invalid") {
		t.Fatal("legacy inventory fields lost")
	}
	if err := SetupHome(dir, ""); err != nil {
		t.Fatal(err)
	}
	after, _ := os.ReadFile(cfg.InventoryPath)
	if !bytes.Equal(before, after) {
		t.Fatal("reinstall modified existing configuration")
	}
	inv, err = LoadInventory(cfg.InventoryPath)
	if err != nil {
		t.Fatal(err)
	}
	for _, profile := range inv.Profiles {
		if profile.DefaultDirectory != custom {
			t.Fatalf("custom path lost %+v", profile)
		}
	}
}

func TestSetupHomePreservesLegacyConfigAndRefusesUnsafeInput(t *testing.T) {
	root, _ := filepath.EvalSymlinks(t.TempDir())
	dir := filepath.Join(root, "config")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HOME", root)
	legacy := []byte("schema_version = 1\nrole = \"home\"\ninventory_path = \"" + filepath.Join(dir, "profiles.toml") + "\"\nstate_dir = \"" + filepath.Join(root, "old-state") + "\"\nclient_id = \"old\"\n")
	path := filepath.Join(dir, "client.toml")
	if err := os.WriteFile(path, legacy, 0600); err != nil {
		t.Fatal(err)
	}
	if err := SetupHome(dir, "~/projects"); err != nil {
		t.Fatal(err)
	}
	after, _ := os.ReadFile(path)
	if !bytes.Equal(legacy, after) {
		t.Fatal("legacy config rewritten")
	}
	if _, err := os.Lstat(filepath.Join(dir, "home.toml")); !os.IsNotExist(err) {
		t.Fatal("shadowed legacy config")
	}
	for _, input := range []string{"/", "relative/path", "~/bad\npath"} {
		if err := SetupHome(dir, input); err == nil {
			t.Errorf("accepted %q", input)
		}
	}
	inventory := filepath.Join(dir, "profiles.toml")
	original, _ := os.ReadFile(inventory)
	target := filepath.Join(root, "victim")
	if err := os.WriteFile(target, original, 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(inventory); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, inventory); err != nil {
		t.Fatal(err)
	}
	if err := SetupHome(dir, "~/other"); err == nil {
		t.Fatal("accepted inventory symlink")
	}
	after, _ = os.ReadFile(target)
	if !bytes.Equal(original, after) {
		t.Fatal("modified symlink target")
	}
}

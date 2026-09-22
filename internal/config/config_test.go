package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestLoadHomeRejectsUnknownFields(t *testing.T) {
	path := filepath.Join(t.TempDir(), "home.toml")
	data := "schema_version = 1\nrole = \"home\"\ntimeout_seconds = 10\nupdate_check = false\nunknown_option = true\n"
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadHome(path); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("expected unknown field error, got %v", err)
	}
}

func TestLoadInventoryRejectsUnknownFields(t *testing.T) {
	path := filepath.Join(t.TempDir(), "inventory.toml")
	data := `
schema_version = 1
revision = "test"
unknown_option = true

[[clients]]
id = "home-mac"
role = "home"

[[identity_refs]]
id = "key"
path = "~/.ssh/test_key"

[[hosts]]
id = "home"
ssh_alias = "hmux-home"
address = "home.invalid"
user = "user"
port = 22
identity_ref = "key"
`
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadInventory(path); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("expected unknown field error, got %v", err)
	}
}

func TestLoadConfigRejectsSymlinkAndWritableFiles(t *testing.T) {
	dir := t.TempDir()
	target := filepath.Join(dir, "target.toml")
	if err := os.WriteFile(target, []byte("schema_version = 1\nrole = \"home\"\ntimeout_seconds = 10\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(dir, "home.toml")
	if err := os.Symlink(target, link); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadHome(link); err == nil || !strings.Contains(err.Error(), "non-symlink") {
		t.Fatalf("expected symlink rejection, got %v", err)
	}
	if err := os.Remove(link); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(link, []byte("schema_version = 1\nrole = \"home\"\ntimeout_seconds = 10\n"), 0o622); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(link, 0o622); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadHome(link); err == nil || !strings.Contains(err.Error(), "writable") {
		t.Fatalf("expected writable config rejection, got %v", err)
	}
}

func TestAtomicWriteRejectsSymlinkDirectory(t *testing.T) {
	dir := t.TempDir()
	realDir := filepath.Join(dir, "real")
	if err := os.Mkdir(realDir, 0o700); err != nil {
		t.Fatal(err)
	}
	linkDir := filepath.Join(dir, "link")
	if err := os.Symlink(realDir, linkDir); err != nil {
		t.Fatal(err)
	}
	if err := AtomicWrite(filepath.Join(linkDir, "value"), []byte("data"), 0o600); err == nil {
		t.Fatal("atomic write followed a symlink directory")
	}
}

func TestAtomicWriteRejectsWritableDirectory(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "unsafe")
	if err := os.Mkdir(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(dir, 0o722); err != nil {
		t.Fatal(err)
	}
	if err := AtomicWrite(filepath.Join(dir, "value"), []byte("data"), 0o600); err == nil ||
		!strings.Contains(err.Error(), "writable") {
		t.Fatalf("expected writable directory rejection, got %v", err)
	}
}

func TestBackupRejectsSymlinkSource(t *testing.T) {
	dir := t.TempDir()
	target := filepath.Join(dir, "target")
	link := filepath.Join(dir, "link")
	if err := os.WriteFile(target, []byte("sensitive"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, link); err != nil {
		t.Fatal(err)
	}
	if _, err := Backup(link, time.Unix(1700000000, 0)); err == nil {
		t.Fatal("symlinked backup source was accepted")
	}
}

func TestHomeConfigurationMigration(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	dir := filepath.Join(home, ".config", "hmux")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	legacy := `schema_version = 1
role = "home"
client_id = "former-home"
dmz_alias = "unused"
home_alias = "unused"
agent_path = "ignored; never executed"
control_path = "unused"
cache_dir = "unused"
public_key_path = "unused"
timeout_seconds = 10
update_check = true
state_dir = "~/private-state"
`
	legacyPath := filepath.Join(dir, "client.toml")
	if err := os.WriteFile(legacyPath, []byte(legacy), 0600); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadHome("")
	if err != nil || cfg.StateDir != filepath.Join(home, "private-state") {
		t.Fatalf("legacy configuration lost: %#v %v", cfg, err)
	}
	raw, _ := os.ReadFile(legacyPath)
	if string(raw) != legacy {
		t.Fatal("loader mutated existing config")
	}
	current := filepath.Join(dir, "home.toml")
	if err := os.WriteFile(current, []byte("schema_version = 1\nstate_dir = \"~/new-state\"\n"), 0600); err != nil {
		t.Fatal(err)
	}
	cfg, err = LoadHome("")
	if err != nil || cfg.StateDir != filepath.Join(home, "new-state") {
		t.Fatalf("new configuration not preferred: %#v %v", cfg, err)
	}
	for _, bad := range []string{"role = \"remote\"\n", "state_dir = \"/\"\n", "inventory_path = \"relative\"\n", "state_diir = \"/tmp/state\"\n"} {
		if err := os.WriteFile(current, []byte("schema_version = 1\n"+bad), 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := LoadHome(""); err == nil {
			t.Fatalf("invalid current config fell back to legacy: %s", bad)
		}
	}
}

func TestInventoryLoadsProfilesWithOrWithoutRetiredTopology(t *testing.T) {
	profiles := `schema_version = 1
revision = "test"
[[profiles]]
id = "shell"
label = "Shell"
default_directory = "~"
command = ["sh"]
`
	topology := `
[[clients]]
id = "retired"
role = "remote"
hostnames = ["unused.invalid"]
[[identity_refs]]
id = "retired"
path = "unused"
[[hosts]]
id = "retired"
ssh_alias = "unused"
address = "unused.invalid"
user = "unused"
port = 22
identity_ref = "retired"
`
	path := filepath.Join(t.TempDir(), "inventory.toml")
	for _, data := range []string{profiles, profiles + topology} {
		if err := os.WriteFile(path, []byte(data), 0600); err != nil {
			t.Fatal(err)
		}
		inventory, err := LoadInventory(path)
		if err != nil || len(inventory.Profiles) != 1 || inventory.Profiles[0].ID != "shell" {
			t.Fatalf("profile load: %#v %v", inventory, err)
		}
	}
	// Compatibility must not silently swallow unknown fields in retired records.
	if err := os.WriteFile(path, []byte(profiles+topology+"invented_option = true\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadInventory(path); err == nil {
		t.Fatal("unknown legacy field accepted")
	}
}

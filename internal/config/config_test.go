package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestLoadClientRejectsUnknownFields(t *testing.T) {
	path := filepath.Join(t.TempDir(), "client.toml")
	data := "schema_version = 1\nrole = \"home\"\ntimeout_seconds = 10\nupdate_check = false\nunknown_option = true\n"
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadClient(path); err == nil || !strings.Contains(err.Error(), "unknown field") {
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
	link := filepath.Join(dir, "client.toml")
	if err := os.Symlink(target, link); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadClient(link); err == nil || !strings.Contains(err.Error(), "non-symlink") {
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
	if _, err := LoadClient(link); err == nil || !strings.Contains(err.Error(), "writable") {
		t.Fatalf("expected writable config rejection, got %v", err)
	}
}

func TestLoadClientRejectsUnsafeRemoteExecutablePath(t *testing.T) {
	path := filepath.Join(t.TempDir(), "client.toml")
	data := "schema_version = 1\nrole = \"home\"\ntimeout_seconds = 10\nagent_path = \"~/.local/bin/agent;touch\"\n"
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadClient(path); err == nil || !strings.Contains(err.Error(), "agent_path") {
		t.Fatalf("expected unsafe agent path rejection, got %v", err)
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

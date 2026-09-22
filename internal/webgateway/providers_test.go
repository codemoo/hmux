package webgateway

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/providers"
)

const testInventory = `schema_version = 1
revision = "test"

[[profiles]]
id = "shell"
label = "Shell"
default_directory = "~"
command = ["sh"]
`

func providerTestConfig(t *testing.T) (config.HomeConfig, providers.Env) {
	t.Helper()
	home := t.TempDir()
	env := providers.Env{Home: home, Path: "/usr/bin:/bin", Timeout: 5 * time.Second, SystemDirs: []string{}}
	old := providerEnv
	providerEnv = func() (providers.Env, error) { return env, nil }
	t.Cleanup(func() { providerEnv = old })
	inventory := filepath.Join(home, ".config", "hmux", "inventory.toml")
	if err := config.AtomicWrite(inventory, []byte(testInventory), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg := config.DefaultHomeConfig()
	cfg.InventoryPath = inventory
	cfg.StateDir = filepath.Join(home, "state")
	return cfg, env
}

func TestProviderActionReportsStatusWithoutSecrets(t *testing.T) {
	cfg, env := providerTestConfig(t)
	key := "AIzaTest00000000009999"
	if err := os.MkdirAll(filepath.Join(env.Home, ".gemini"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(env.Home, ".gemini", ".env"), []byte("GEMINI_API_KEY="+key+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	value, err := providerAction(context.Background(), cfg, "providers", nil)
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := json.Marshal(value)
	if strings.Contains(string(raw), key) || !strings.Contains(string(raw), `"key_hint":"…9999"`) {
		t.Fatalf("providers response = %s", raw)
	}
	for _, s := range value.(providerResult).Providers {
		if s.Installed || s.Profile || s.ProfileID != "" {
			t.Fatalf("uninstalled provider = %+v", s)
		}
	}
	if inventory, _ := config.LoadInventory(cfg.InventoryPath); len(inventory.Profiles) != 1 {
		t.Fatalf("profiles added without installs: %+v", inventory.Profiles)
	}
}

func TestProviderActionRegistersInstalledCLIsOnce(t *testing.T) {
	cfg, env := providerTestConfig(t)
	bin := filepath.Join(env.Home, ".local", "bin")
	if err := os.MkdirAll(bin, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bin, "gemini"), []byte("#!/bin/sh\necho 0.60.0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	for range 2 {
		value, err := providerAction(context.Background(), cfg, "providers", nil)
		if err != nil {
			t.Fatal(err)
		}
		for _, s := range value.(providerResult).Providers {
			want := s.ID == "gemini"
			if s.Installed != want || s.Profile != want || (s.ProfileID == "gemini") != want {
				t.Fatalf("status = %+v", s)
			}
		}
	}
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil {
		t.Fatal(err)
	}
	last := inventory.Profiles[len(inventory.Profiles)-1]
	if len(inventory.Profiles) != 2 || last.ID != "gemini" || last.Command[0] != "gemini" || last.DefaultDirectory != "~" {
		t.Fatalf("profiles = %+v", inventory.Profiles)
	}
	if backups, _ := filepath.Glob(cfg.InventoryPath + ".hmux-backup-*"); len(backups) != 1 {
		t.Fatalf("backups = %v", backups)
	}
}

func TestProviderActionRejectsMalformedRequests(t *testing.T) {
	cfg, _ := providerTestConfig(t)
	for operation, payload := range map[string]string{
		"provider-key":   `{"provider":"gemini","key":"AIzaTest00000000000000","extra":1}`,
		"provider-setup": `{"provider":"gemini","action":"uninstall"}`,
		"providers":      `{"x":1}`,
	} {
		if _, err := providerAction(context.Background(), cfg, operation, json.RawMessage(payload)); err == nil {
			t.Fatalf("%s accepted %s", operation, payload)
		}
	}
	value, err := providerAction(context.Background(), cfg, "provider-key", json.RawMessage(`{"provider":"gemini","key":"bad key"}`))
	if err != nil || value.(providerResult).Error == "" {
		t.Fatalf("invalid key: %v %+v", err, value)
	}
	value, err = providerAction(context.Background(), cfg, "provider-job-start", json.RawMessage(`{"provider":"gemini","action":"uninstall"}`))
	if err != nil || value.(providerResult).Error == "" {
		t.Fatalf("unknown job action: %v %+v", err, value)
	}
	cfg.Role = "remote"
	value, err = providerAction(context.Background(), cfg, "providers", nil)
	if err != nil || value.(providerResult).Error == "" {
		t.Fatalf("remote role: %v %+v", err, value)
	}
}

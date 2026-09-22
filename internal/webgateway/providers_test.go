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
	"github.com/codemoo/hmux/internal/model"
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

func TestProviderStatusDoesNotModifyInventory(t *testing.T) {
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
			installed := s.ID == "gemini"
			if s.Installed != installed || s.Profile || s.ProfileID != "" {
				t.Fatalf("status = %+v", s)
			}
		}
	}
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil {
		t.Fatal(err)
	}
	if len(inventory.Profiles) != 1 {
		t.Fatalf("profiles = %+v", inventory.Profiles)
	}
	if backups, _ := filepath.Glob(cfg.InventoryPath + ".hmux-backup-*"); len(backups) != 0 {
		t.Fatalf("backups = %v", backups)
	}
}

func TestProviderKeyAddsProfileUsingConfiguredWorkspace(t *testing.T) {
	cfg, env := providerTestConfig(t)
	bin := filepath.Join(env.Home, ".local", "bin")
	if err := os.MkdirAll(bin, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bin, "gemini"), []byte("#!/bin/sh\necho 0.60.0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil {
		t.Fatal(err)
	}
	inventory.Profiles[0].DefaultDirectory = "~/Dropbox/dev"
	if err := config.SaveInventory(cfg.InventoryPath, inventory); err != nil {
		t.Fatal(err)
	}
	file, err := os.OpenFile(cfg.InventoryPath, os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := file.WriteString("\n[[clients]]\nid = \"retired-home\"\nrole = \"home\"\nhostnames = [\"fixture.invalid\"]\n"); err != nil {
		t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	value, err := providerAction(context.Background(), cfg, "provider-key", json.RawMessage(`{"provider":"gemini","key":"AIzaTest00000000009999"}`))
	if err != nil || value.(providerResult).Error != "" {
		t.Fatalf("provider key: value=%+v err=%v", value, err)
	}
	inventory, err = config.LoadInventory(cfg.InventoryPath)
	if err != nil {
		t.Fatal(err)
	}
	last := inventory.Profiles[len(inventory.Profiles)-1]
	if len(inventory.Profiles) != 2 || last.ID != "gemini" || last.Command[0] != "gemini" || last.DefaultDirectory != "~/Dropbox/dev" {
		t.Fatalf("profiles = %+v", inventory.Profiles)
	}
	raw, err := os.ReadFile(cfg.InventoryPath)
	if err != nil || !strings.Contains(string(raw), "fixture.invalid") {
		t.Fatalf("retired inventory fields lost: %s err=%v", raw, err)
	}
	if backups, _ := filepath.Glob(cfg.InventoryPath + ".hmux-backup-*"); len(backups) != 1 {
		t.Fatalf("backups = %v", backups)
	}
}

func TestConcurrentProviderProfileAddsAreIdempotent(t *testing.T) {
	cfg, env := providerTestConfig(t)
	bin := filepath.Join(env.Home, ".local", "bin")
	if err := os.MkdirAll(bin, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bin, "gemini"), []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	errs := make(chan error, 8)
	for range 8 {
		go func() { errs <- ensureProviderProfile(context.Background(), cfg.InventoryPath, env, "gemini") }()
	}
	for range 8 {
		if err := <-errs; err != nil {
			t.Fatal(err)
		}
	}
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil || len(inventory.Profiles) != 2 {
		t.Fatalf("profiles=%+v err=%v", inventory.Profiles, err)
	}
}

func TestProviderProfileRejectsUnsafeInventoryLock(t *testing.T) {
	cfg, env := providerTestConfig(t)
	bin := filepath.Join(env.Home, ".local", "bin")
	if err := os.MkdirAll(bin, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bin, "gemini"), []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(env.Home, "lock-target")
	if err := os.WriteFile(target, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, cfg.InventoryPath+".lock"); err != nil {
		t.Fatal(err)
	}
	if err := ensureProviderProfile(context.Background(), cfg.InventoryPath, env, "gemini"); err == nil {
		t.Fatal("accepted inventory lock symlink")
	}
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil || len(inventory.Profiles) != 1 {
		t.Fatalf("profiles=%+v err=%v", inventory.Profiles, err)
	}
}

func TestInventoryWorkspaceBaseUsesEstablishedRoot(t *testing.T) {
	inventory := model.Inventory{Profiles: []model.Profile{
		{DefaultDirectory: "~/Dropbox/dev"},
		{DefaultDirectory: "~"},
		{DefaultDirectory: "~/Dropbox/dev"},
	}}
	if got := inventoryWorkspaceBase(inventory); got != "~/Dropbox/dev" {
		t.Fatalf("workspace base = %q", got)
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

func TestChangesProviderAuth(t *testing.T) {
	job := func(state string) providerResult {
		return providerResult{Job: &providers.JobStatus{State: state}}
	}
	for _, tc := range []struct {
		operation string
		data      any
		want      bool
	}{
		{"provider-key", providerResult{}, true},
		{"provider-key", providerResult{Error: "bad"}, false},
		{"provider-job", job(providers.JobConnected), true},
		{"provider-job-input", job(providers.JobDone), true},
		{"provider-job", job(providers.JobLogin), false},
		{"provider-job", job(providers.JobFailed), false},
		{"providers", providerResult{}, false},
		{"create", map[string]string{}, false},
	} {
		if got := changesProviderAuth(tc.operation, tc.data); got != tc.want {
			t.Fatalf("%s %+v = %v", tc.operation, tc.data, got)
		}
	}
}

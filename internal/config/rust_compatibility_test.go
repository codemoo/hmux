package config

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// This oracle records the actual Go read-only loader result for synthetic files.
// UPDATE_HMUX_RUST_CONFIG_FIXTURE=1 regenerates only the tracked fixture.
func TestRustReadOnlyConfigOracle(t *testing.T) {
	fixtureDir := filepath.Join("..", "..", "tests", "fixtures", "config-v1")
	home := t.TempDir()
	t.Setenv("HOME", home)
	result := map[string]any{}
	defaultConfig, err := LoadHome(filepath.Join(home, "missing-home.toml"))
	if err != nil {
		t.Fatal(err)
	}
	result["home-default"] = map[string]any{
		"schema_version": defaultConfig.SchemaVersion,
		"role":           defaultConfig.Role,
		"inventory_path": strings.ReplaceAll(defaultConfig.InventoryPath, home, "{HOME}"),
		"state_dir":      strings.ReplaceAll(defaultConfig.StateDir, home, "{HOME}"),
	}
	for _, name := range []string{"home-current", "home-legacy"} {
		config, err := LoadHome(filepath.Join(fixtureDir, name+".toml"))
		if err != nil {
			t.Fatal(err)
		}
		result[name] = map[string]any{
			"schema_version": config.SchemaVersion,
			"role":           config.Role,
			"inventory_path": strings.ReplaceAll(config.InventoryPath, home, "{HOME}"),
			"state_dir":      strings.ReplaceAll(config.StateDir, home, "{HOME}"),
		}
	}
	inventory, err := LoadInventory(filepath.Join(fixtureDir, "inventory.toml"))
	if err != nil {
		t.Fatal(err)
	}
	result["inventory"] = inventory
	raw, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	raw = append(raw, '\n')
	path := filepath.Join(fixtureDir, "go-oracle.json")
	if os.Getenv("UPDATE_HMUX_RUST_CONFIG_FIXTURE") == "1" {
		if err := os.WriteFile(path, raw, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(raw, want) {
		t.Fatalf("Go config oracle changed; review and regenerate %s", path)
	}
}

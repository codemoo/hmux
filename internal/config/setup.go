package config

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"
	"unicode"

	"github.com/BurntSushi/toml"
	"github.com/codemoo/hmux/internal/model"
)

// SetupHome initializes local configuration. An empty workspace preserves all
// existing profile directories; only a new inventory defaults to ~/.hmux.
// Explicit changes retain a timestamped backup and all legacy inventory fields.
func SetupHome(directory, workspace string) error {
	directory, err := expandUserPath(directory)
	if err != nil {
		return err
	}
	if !filepath.IsAbs(directory) {
		return errors.New("config directory must be absolute")
	}
	directory = filepath.Clean(directory)
	if err := trustedSetupDirectory(directory); err != nil {
		return err
	}
	homePath := filepath.Join(directory, "home.toml")
	configPath := homePath
	if _, err := os.Lstat(homePath); errors.Is(err, os.ErrNotExist) {
		configPath = filepath.Join(directory, "client.toml")
	}
	_, err = os.Lstat(configPath)
	newHome := errors.Is(err, os.ErrNotExist)
	if err != nil && !newHome {
		return err
	}
	cfg := DefaultHomeConfig()
	cfg.InventoryPath = filepath.Join(directory, "inventory.toml")
	if !newHome {
		cfg, err = LoadHome(configPath)
		if err != nil {
			return err
		}
	}
	if err := trustedSetupDirectory(filepath.Dir(cfg.InventoryPath)); err != nil {
		return err
	}
	_, err = os.Lstat(cfg.InventoryPath)
	newInventory := errors.Is(err, os.ErrNotExist)
	if err != nil && !newInventory {
		return err
	}
	var inventory model.Inventory
	if !newInventory {
		inventory, err = LoadInventory(cfg.InventoryPath)
		if err != nil {
			return err
		}
	}
	if workspace == "" && newInventory {
		workspace = "~/.hmux"
	}
	if workspace != "" {
		expanded, err := expandUserPath(workspace)
		if err != nil {
			return err
		}
		if !filepath.IsAbs(expanded) || filepath.Clean(expanded) == "/" || len(workspace) > 4096 || strings.IndexFunc(workspace, unicode.IsControl) >= 0 {
			return errors.New("workspace directory must be an absolute user path or start with ~/")
		}
		if info, err := os.Stat(expanded); err == nil && !info.IsDir() {
			return errors.New("workspace path is not a directory")
		} else if err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
	}
	if newInventory {
		shell := "sh"
		if _, err := exec.LookPath("zsh"); err == nil {
			shell = "zsh"
		}
		inventory = model.Inventory{SchemaVersion: model.SchemaVersion, Revision: "installed", Profiles: []model.Profile{
			{ID: "codex", Label: "Codex", DefaultDirectory: workspace, Command: []string{"codex"}, Tags: []string{"ai", "codex"}},
			{ID: "claude", Label: "Claude Code", DefaultDirectory: workspace, Command: []string{"claude"}, Tags: []string{"ai", "claude"}},
			{ID: "shell", Label: "Shell", DefaultDirectory: workspace, Command: []string{shell, "-l"}, Tags: []string{"shell"}},
		}}
		if err := SaveInventory(cfg.InventoryPath, inventory); err != nil {
			return err
		}
	} else if workspace != "" {
		changed := false
		for _, profile := range inventory.Profiles {
			changed = changed || profile.DefaultDirectory != workspace
		}
		if changed {
			// Decode into maps to preserve retired topology fields instead of dropping
			// them while updating only the administrator-selected profile paths.
			var raw map[string]any
			if _, err := toml.DecodeFile(cfg.InventoryPath, &raw); err != nil {
				return err
			}
			profiles, ok := raw["profiles"].([]map[string]any)
			if !ok {
				return errors.New("invalid inventory profile tables")
			}
			for _, profile := range profiles {
				profile["default_directory"] = workspace
			}
			var output bytes.Buffer
			if err := toml.NewEncoder(&output).Encode(raw); err != nil {
				return err
			}
			if _, err := Backup(cfg.InventoryPath, time.Now()); err != nil {
				return err
			}
			if err := AtomicWrite(cfg.InventoryPath, output.Bytes(), 0600); err != nil {
				return err
			}
		}
	}
	if newHome {
		var output bytes.Buffer
		if err := toml.NewEncoder(&output).Encode(cfg); err != nil {
			return err
		}
		return AtomicWrite(homePath, output.Bytes(), 0600)
	}
	return nil
}

// Inspect existing ancestors before creating/writing config. macOS's system
// /var and /tmp links are resolved by callers selecting the directory, not by
// traversing user-controlled config links here.
func trustedSetupDirectory(path string) error {
	for at := path; ; at = filepath.Dir(at) {
		info, err := os.Lstat(at)
		if err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		if err == nil {
			st, ok := info.Sys().(*syscall.Stat_t)
			if !ok || !info.IsDir() || info.Mode()&os.ModeSymlink != 0 || (st.Uid != 0 && int(st.Uid) != os.Getuid()) || (info.Mode().Perm()&0022 != 0 && !(st.Uid == 0 && info.Mode()&os.ModeSticky != 0)) {
				return fmt.Errorf("untrusted configuration directory: %s", at)
			}
		}
		if filepath.Dir(at) == at {
			break
		}
	}
	return nil
}

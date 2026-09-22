package config

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/BurntSushi/toml"
	"github.com/codemoo/hmux/internal/model"
)

// HomeConfig contains only the local paths used by the web connector.
type HomeConfig struct {
	SchemaVersion int    `toml:"schema_version"`
	Role          string `toml:"role"`
	InventoryPath string `toml:"inventory_path"`
	StateDir      string `toml:"state_dir"`
}

func DefaultHomeConfig() HomeConfig {
	home, _ := os.UserHomeDir()
	return HomeConfig{SchemaVersion: model.SchemaVersion, Role: "home", InventoryPath: filepath.Join(home, ".config", "hmux", "inventory.toml"), StateDir: filepath.Join(home, ".local", "state", "hmux")}
}

// LoadHome prefers home.toml, with read-only compatibility for existing client.toml.
// Retired client keys are accepted but never used to connect or run commands.
func LoadHome(path string) (HomeConfig, error) {
	cfg := DefaultHomeConfig()
	if path == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return cfg, err
		}
		path = filepath.Join(home, ".config", "hmux", "home.toml")
		if _, err = os.Lstat(path); errors.Is(err, os.ErrNotExist) {
			path = filepath.Join(home, ".config", "hmux", "client.toml")
		}
	}
	if _, err := os.Lstat(path); errors.Is(err, os.ErrNotExist) {
		return cfg, nil
	}
	if err := validateConfigFile(path, 1024*1024); err != nil {
		return cfg, fmt.Errorf("Home config: %w", err)
	}
	var wire struct {
		HomeConfig
		ClientID      string `toml:"client_id"`
		DMZAlias      string `toml:"dmz_alias"`
		HomeAlias     string `toml:"home_alias"`
		AgentPath     string `toml:"agent_path"`
		ControlPath   string `toml:"control_path"`
		CacheDir      string `toml:"cache_dir"`
		PublicKeyPath string `toml:"public_key_path"`
		Timeout       int    `toml:"timeout_seconds"`
		UpdateCheck   bool   `toml:"update_check"`
	}
	wire.HomeConfig = cfg
	metadata, err := toml.DecodeFile(path, &wire)
	if err != nil {
		return cfg, fmt.Errorf("decode Home config: %w", err)
	}
	if unknown := metadata.Undecoded(); len(unknown) > 0 {
		return cfg, fmt.Errorf("decode Home config: unknown field %q", unknown[0].String())
	}
	cfg = wire.HomeConfig
	if cfg.SchemaVersion != model.SchemaVersion {
		return cfg, fmt.Errorf("Home schema_version must be %d", model.SchemaVersion)
	}
	if cfg.Role != "home" {
		return cfg, errors.New("HMux supports web/PWA clients only; run the connector on the Home host with role = home")
	}
	for _, path := range []*string{&cfg.InventoryPath, &cfg.StateDir} {
		expanded, err := expandUserPath(*path)
		if err != nil {
			return cfg, err
		}
		cleaned := filepath.Clean(expanded)
		if !filepath.IsAbs(cleaned) || cleaned == string(os.PathSeparator) {
			return cfg, errors.New("Home paths must be absolute user paths, not the filesystem root")
		}
		*path = cleaned
	}
	return cfg, nil
}

func expandUserPath(path string) (string, error) {
	if path != "~" && !strings.HasPrefix(path, "~/") {
		return path, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	if path == "~" {
		return home, nil
	}
	return filepath.Join(home, strings.TrimPrefix(path, "~/")), nil
}

func LoadInventory(path string) (model.Inventory, error) {
	var inventory model.Inventory
	if err := validateConfigFile(path, 16*1024*1024); err != nil {
		return inventory, fmt.Errorf("inventory: %w", err)
	}
	var wire struct {
		model.Inventory
		Clients      []legacyClient      `toml:"clients"`
		IdentityRefs []legacyIdentityRef `toml:"identity_refs"`
		Hosts        []legacyHost        `toml:"hosts"`
	}
	metadata, err := toml.DecodeFile(path, &wire)
	inventory = wire.Inventory
	if err != nil {
		return inventory, fmt.Errorf("decode inventory: %w", err)
	}
	if undecoded := metadata.Undecoded(); len(undecoded) > 0 {
		return inventory, fmt.Errorf("decode inventory: unknown field %q", undecoded[0].String())
	}
	if err := inventory.Validate(); err != nil {
		return inventory, fmt.Errorf("validate inventory: %w", err)
	}
	return inventory, nil
}

func validateConfigFile(path string, maxSize int64) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
		return errors.New("must be a regular non-symlink file")
	}
	if info.Size() < 1 || info.Size() > maxSize {
		return fmt.Errorf("size must be between 1 and %d bytes", maxSize)
	}
	if info.Mode().Perm()&0o022 != 0 {
		return errors.New("must not be group/world writable")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("must be owned by the current user")
	}
	return nil
}

func SaveInventory(path string, inventory model.Inventory) error {
	if err := inventory.Validate(); err != nil {
		return err
	}
	var out bytes.Buffer
	if err := toml.NewEncoder(&out).Encode(inventory); err != nil {
		return fmt.Errorf("encode inventory: %w", err)
	}
	return AtomicWrite(path, out.Bytes(), 0o600)
}

// AppendInventoryProfile adds one profile while retaining retired compatibility
// tables as inert data. Callers serialize the read-modify-write operation.
func AppendInventoryProfile(path string, profile model.Profile, now time.Time) error {
	inventory, err := LoadInventory(path)
	if err != nil {
		return err
	}
	inventory.Profiles = append(inventory.Profiles, profile)
	if err := inventory.Validate(); err != nil {
		return err
	}
	var raw map[string]any
	if _, err := toml.DecodeFile(path, &raw); err != nil {
		return err
	}
	profiles, ok := raw["profiles"].([]map[string]any)
	if !ok {
		return errors.New("invalid inventory profile tables")
	}
	profiles = append(profiles, map[string]any{
		"id":                profile.ID,
		"label":             profile.Label,
		"default_directory": profile.DefaultDirectory,
		"command":           profile.Command,
		"tags":              profile.Tags,
	})
	raw["profiles"] = profiles
	var output bytes.Buffer
	if err := toml.NewEncoder(&output).Encode(raw); err != nil {
		return err
	}
	if _, err := Backup(path, now); err != nil {
		return err
	}
	return AtomicWrite(path, output.Bytes(), 0o600)
}

func AtomicWrite(path string, data []byte, mode os.FileMode) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return errors.New("atomic write directory must be a real directory, not a symlink")
	}
	if info.Mode().Perm()&0o022 != 0 {
		return errors.New("atomic write directory must not be group/world writable")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return errors.New("atomic write directory must be owned by the current user")
	}
	tmp, err := os.CreateTemp(dir, ".hmux-*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if err := tmp.Chmod(mode); err != nil {
		_ = tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmpName, path); err != nil {
		return err
	}
	return syncAtomicWriteDirectory(dir)
}

func syncAtomicWriteDirectory(dir string) error {
	parent, err := os.Open(dir)
	if err != nil {
		return err
	}
	syncErr := parent.Sync()
	closeErr := parent.Close()
	return errors.Join(syncErr, closeErr)
}

func Backup(path string, now time.Time) (string, error) {
	linkInfo, err := os.Lstat(path)
	if err != nil {
		return "", err
	}
	if linkInfo.Mode()&os.ModeSymlink != 0 || !linkInfo.Mode().IsRegular() {
		return "", errors.New("backup source must be a regular non-symlink file")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	backup := fmt.Sprintf("%s.hmux-backup-%s", path, now.UTC().Format("20060102T150405.000000000Z"))
	if err := AtomicWrite(backup, data, linkInfo.Mode().Perm()); err != nil {
		return "", err
	}
	return backup, nil
}

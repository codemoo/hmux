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

type ClientConfig struct {
	SchemaVersion int    `toml:"schema_version"`
	ClientID      string `toml:"client_id"`
	Role          string `toml:"role"`
	DMZAlias      string `toml:"dmz_alias"`
	HomeAlias     string `toml:"home_alias"`
	AgentPath     string `toml:"agent_path"`
	ControlPath   string `toml:"control_path"`
	InventoryPath string `toml:"inventory_path"`
	CacheDir      string `toml:"cache_dir"`
	StateDir      string `toml:"state_dir"`
	PublicKeyPath string `toml:"public_key_path"`
	Timeout       int    `toml:"timeout_seconds"`
	UpdateCheck   bool   `toml:"update_check"`
}

func DefaultClientConfig() ClientConfig {
	home, _ := os.UserHomeDir()
	return ClientConfig{
		SchemaVersion: model.SchemaVersion,
		ClientID:      "home-mac",
		Role:          "home",
		DMZAlias:      "hmux-dmz",
		HomeAlias:     "hmux-home",
		AgentPath:     "~/.local/bin/hmux-agent",
		ControlPath:   "~/.local/bin/hmux-control",
		InventoryPath: filepath.Join(home, ".config", "hmux", "inventory.toml"),
		CacheDir:      filepath.Join(home, ".cache", "hmux"),
		StateDir:      filepath.Join(home, ".local", "state", "hmux"),
		PublicKeyPath: filepath.Join(home, ".config", "hmux", "release-public-key.pem"),
		Timeout:       10,
		UpdateCheck:   false,
	}
}

func LoadClient(path string) (ClientConfig, error) {
	cfg := DefaultClientConfig()
	if path == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return cfg, err
		}
		path = filepath.Join(home, ".config", "hmux", "client.toml")
	}
	if _, err := os.Lstat(path); errors.Is(err, os.ErrNotExist) {
		return cfg, nil
	}
	if err := validateConfigFile(path, 1024*1024); err != nil {
		return cfg, fmt.Errorf("client config: %w", err)
	}
	metadata, err := toml.DecodeFile(path, &cfg)
	if err != nil {
		return cfg, fmt.Errorf("decode client config: %w", err)
	}
	if undecoded := metadata.Undecoded(); len(undecoded) > 0 {
		return cfg, fmt.Errorf("decode client config: unknown field %q", undecoded[0].String())
	}
	if cfg.SchemaVersion != model.SchemaVersion {
		return cfg, fmt.Errorf("client schema_version must be %d", model.SchemaVersion)
	}
	if cfg.Role != "home" && cfg.Role != "remote" {
		return cfg, fmt.Errorf("client role must be home or remote")
	}
	if err := model.ValidateStableID(cfg.ClientID); err != nil {
		return cfg, fmt.Errorf("client_id: %w", err)
	}
	if cfg.Timeout < 1 || cfg.Timeout > 120 {
		return cfg, fmt.Errorf("timeout_seconds must be between 1 and 120")
	}
	for _, path := range []*string{&cfg.InventoryPath, &cfg.CacheDir, &cfg.StateDir, &cfg.PublicKeyPath} {
		expanded, err := expandUserPath(*path)
		if err != nil {
			return cfg, err
		}
		*path = expanded
		cleaned := filepath.Clean(expanded)
		if !filepath.IsAbs(cleaned) || cleaned == string(os.PathSeparator) {
			return cfg, errors.New("client paths must be absolute user paths, not the filesystem root")
		}
		*path = cleaned
	}
	for name, value := range map[string]string{
		"dmz_alias": cfg.DMZAlias, "home_alias": cfg.HomeAlias,
	} {
		if !validSSHAlias(value) {
			return cfg, fmt.Errorf("%s is invalid", name)
		}
	}
	for name, value := range map[string]string{
		"agent_path": cfg.AgentPath, "control_path": cfg.ControlPath,
	} {
		if !validRemotePath(value) {
			return cfg, fmt.Errorf("%s is invalid", name)
		}
	}
	return cfg, nil
}

func validSSHAlias(value string) bool {
	if value == "" || len(value) > 128 || value[0] == '-' {
		return false
	}
	for _, r := range value {
		if !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
			(r >= '0' && r <= '9') || strings.ContainsRune("._-", r)) {
			return false
		}
	}
	return true
}

func validRemotePath(value string) bool {
	if value == "" || len(value) > 512 || strings.Contains(value, "..") ||
		(!strings.HasPrefix(value, "~/") && !strings.HasPrefix(value, "/")) {
		return false
	}
	for _, r := range value {
		if !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
			(r >= '0' && r <= '9') || strings.ContainsRune("~/_-.", r)) {
			return false
		}
	}
	return true
}

func SaveClient(path string, cfg ClientConfig) error {
	var out bytes.Buffer
	if err := toml.NewEncoder(&out).Encode(cfg); err != nil {
		return fmt.Errorf("encode client config: %w", err)
	}
	return AtomicWrite(path, out.Bytes(), 0o600)
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
	metadata, err := toml.DecodeFile(path, &inventory)
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

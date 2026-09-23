package homeservice

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	"github.com/codemoo/hmux/internal/config"
)

func (m manager) install(ctx context.Context, endpoint, token, configPath, binaryTarget string, adopt bool, out io.Writer) error {
	all, err := m.connectorProcesses()
	if err != nil {
		return fmt.Errorf("cannot verify existing connectors: %w", err)
	}
	managedPID := m.pid(ctx)
	var previous *process
	if adopt {
		if len(all) != 1 {
			return fmt.Errorf("--from-running requires exactly one Home connector owned by this user; found %d", len(all))
		}
		previous = &all[0]
		endpoint, token, configPath, err = connectorOptions(*previous)
		if err != nil {
			return err
		}
	} else if len(all) > 0 {
		if len(all) != 1 || managedPID == 0 || all[0].PID != managedPID {
			return errors.New("a manual Home connector is running; use service install --from-running to adopt it")
		}
	}
	u, err := url.Parse(endpoint)
	if err != nil || u.Scheme != "wss" || u.Hostname() == "" || u.Path != "/connect" || u.RawQuery != "" || u.Fragment != "" || u.User != nil || !cleanValue(endpoint) {
		return errors.New("--url must be wss://host/connect")
	}
	if token == "" {
		return errors.New("--token-file is required")
	}
	token, err = filepath.Abs(token)
	if err != nil {
		return err
	}
	if err := trustedDirectory(filepath.Dir(token), false); err != nil {
		return err
	}
	raw, err := readOwned(token, 256, true)
	if err != nil {
		return err
	}
	decoded, err := base64.RawURLEncoding.DecodeString(strings.TrimSpace(string(raw)))
	if err != nil || len(decoded) != 32 {
		return errors.New("invalid connector token file")
	}
	if configPath == "" {
		configPath = filepath.Join(m.home, ".config", "hmux", "home.toml")
		if _, err := os.Lstat(configPath); errors.Is(err, os.ErrNotExist) {
			configPath = filepath.Join(m.home, ".config", "hmux", "client.toml")
		}
	}
	configPath, err = filepath.Abs(configPath)
	if err != nil {
		return err
	}
	if _, err := readOwned(configPath, 1<<20, false); err != nil {
		return fmt.Errorf("existing Home config required (run hmux-agent setup-home for a new installation): %w", err)
	}
	cfg, err := config.LoadHome(configPath)
	if err != nil {
		return err
	}
	if _, err := config.LoadInventory(cfg.InventoryPath); err != nil {
		return err
	}
	env, err := serviceEnvironment(m.home)
	if previous != nil {
		env, err = serviceEnvironmentFrom(m.home, previous.Environment)
	}
	if err != nil {
		return err
	}
	if !executableInPath("tmux", env["PATH"]) {
		return errors.New("tmux is missing from the service PATH; install from the terminal where your provider CLIs work")
	}
	if err := trustedDirectory(cfg.StateDir, true); err != nil {
		return err
	}
	executable, err := os.Executable()
	if err != nil {
		return err
	}
	executable, err = filepath.EvalSymlinks(executable)
	if err != nil {
		return err
	}
	binary, err := readOwned(executable, 256<<20, false)
	if err != nil {
		return err
	}
	if binaryTarget == "" {
		binaryTarget = filepath.Join(m.home, ".local", "bin", "hmux-web")
	}
	binaryTarget, err = filepath.Abs(binaryTarget)
	if err != nil || filepath.Base(binaryTarget) != "hmux-web" {
		return errors.New("service binary destination must be an absolute path ending in hmux-web")
	}
	s := Spec{Binary: binaryTarget, Home: m.home, Endpoint: endpoint, TokenFile: token, ConfigFile: configPath, LogFile: filepath.Join(cfg.StateDir, "home-service.log"), Environment: env}
	for _, value := range []string{s.Binary, s.Home, s.TokenFile, s.LogFile} {
		if !cleanValue(value) {
			return errors.New("invalid service path")
		}
	}
	if configPath != "" && !cleanValue(configPath) {
		return errors.New("invalid config path")
	}
	if _, err := os.Lstat(m.path); err == nil {
		if err := m.checkInstalled(); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err := trustedDirectory(filepath.Dir(s.Binary), true); err != nil {
		return err
	}
	if _, err := os.Lstat(s.Binary); err == nil {
		if _, err := readOwned(s.Binary, 256<<20, false); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	// Preflight logging before stopping a live connector. No gateway contact yet.
	log, err := OpenLog(s.LogFile)
	if err != nil {
		return err
	}
	_ = log.Close()
	data := SystemdUnit(s)
	if m.mac {
		data = LaunchAgent(s)
	}
	if err := writeBackedUp(m.path, data, 0600); err != nil {
		return err
	}
	if executable != s.Binary {
		if err := writeBackedUp(s.Binary, binary, 0755); err != nil {
			return err
		}
	}
	if managedPID != 0 || m.loaded(ctx) {
		if err := m.stop(ctx, false); err != nil {
			return err
		}
	}
	if previous != nil && previous.PID != managedPID {
		if err := stopAdopted(ctx, *previous); err != nil {
			return err
		}
	}
	// Older manually started binaries do not honor the new lock. Recheck after
	// adoption so a changed or respawned process cannot compete with this service.
	if err := m.waitForStop(ctx); err != nil {
		return err
	}
	if err := m.start(ctx); err != nil {
		return fmt.Errorf("service files installed but activation failed; fix the service manager and run hmux-web service start: %w", err)
	}
	fmt.Fprintln(out, "Home service installed and start requested. Run hmux-web service status to check the process.")
	fmt.Fprintln(out, "Bounded service log:", s.LogFile)
	if m.mac {
		fmt.Fprintln(out, "Starts at macOS login and restarts after process exit. The Mac must stay awake for remote access.")
	} else {
		fmt.Fprintln(out, "Starts with the systemd user manager. For boot/logout persistence, an administrator can run: sudo loginctl enable-linger <home-user>")
	}
	return nil
}

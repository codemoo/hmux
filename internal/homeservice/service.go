package homeservice

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/safeexec"
)

const usage = "usage: hmux-web service <install|status|start|stop|restart|uninstall> [--from-running | --url wss://host/connect --token-file /private/token] [--config /private/home.toml]"

type manager struct {
	home, path, command, domain, target string
	mac                                 bool
	execute                             func(context.Context, string, ...string) ([]byte, error)
	processes                           func() ([]process, error)
}

func (m manager) connectorProcesses() ([]process, error) {
	if m.processes != nil {
		return m.processes()
	}
	return connectors()
}

func (m manager) waitForStop(ctx context.Context) error {
	deadline := time.NewTimer(20 * time.Second)
	defer deadline.Stop()
	tick := time.NewTicker(100 * time.Millisecond)
	defer tick.Stop()
	for {
		all, err := m.connectorProcesses()
		if err == nil && len(all) == 0 {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			return errors.New("a Home connector is still running; replacement service not started")
		case <-tick.C:
		}
	}
}

func newManager() (manager, error) {
	if os.Getuid() == 0 {
		return manager{}, errors.New("run Home service management as the user who owns tmux and the provider CLIs, without sudo")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return manager{}, err
	}
	if err := trustedDirectory(home, false); err != nil {
		return manager{}, err
	}
	m := manager{home: home, mac: runtime.GOOS == "darwin"}
	switch runtime.GOOS {
	case "darwin":
		m.command = "/bin/launchctl"
		m.domain = "gui/" + strconv.Itoa(os.Getuid())
		m.target = m.domain + "/" + Label
		m.path = filepath.Join(home, "Library", "LaunchAgents", Label+".plist")
	case "linux":
		for _, candidate := range []string{"/usr/bin/systemctl", "/bin/systemctl"} {
			if systemExecutable(candidate) {
				m.command = candidate
				break
			}
		}
		if m.command == "" {
			return manager{}, errors.New("systemd user services are required on Linux")
		}
		base, err := os.UserConfigDir()
		if err != nil {
			return manager{}, err
		}
		m.path = filepath.Join(base, "systemd", "user", Unit)
	default:
		return manager{}, errors.New("Home services support macOS and Linux")
	}
	return m, nil
}

func (m manager) run(ctx context.Context, args ...string) ([]byte, error) {
	bounded, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	if m.execute != nil {
		return m.execute(bounded, m.command, args...)
	}
	out, err := safeexec.Output(exec.CommandContext(bounded, m.command, args...), 128<<10)
	if err != nil {
		return nil, fmt.Errorf("service manager failed (%s): %w", strings.Join(args, " "), err)
	}
	return out, nil
}

// Domain availability needs only the exit status. A GUI domain can list hundreds
// of services; collecting that output made a healthy domain fail the output cap.
func (m manager) probeDomain(ctx context.Context) error {
	bounded, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	if m.execute != nil {
		_, err := m.execute(bounded, m.command, "print", m.domain)
		return err
	}
	command := exec.CommandContext(bounded, m.command, "print", m.domain)
	command.Stdout = io.Discard
	command.Stderr = io.Discard
	return command.Run()
}

func (m manager) available(ctx context.Context) error {
	if m.mac {
		err := m.probeDomain(ctx)
		if err != nil {
			return fmt.Errorf("cannot access macOS GUI service manager; run from a normal Terminal in the logged-in Home account (sandbox restrictions can also block access): %w", err)
		}
		return nil
	}
	_, err := m.run(ctx, "--user", "show", "--property=Version")
	if err != nil {
		return errors.New("systemd user manager is unavailable; run in the Home user's login session")
	}
	return nil
}

func (m manager) loaded(ctx context.Context) bool {
	if m.mac {
		_, err := m.run(ctx, "print", m.target)
		return err == nil
	}
	_, err := m.run(ctx, "--user", "is-active", "--quiet", Unit)
	return err == nil
}

func (m manager) pid(ctx context.Context) int {
	var out []byte
	var err error
	if m.mac {
		out, err = m.run(ctx, "print", m.target)
		for _, line := range strings.Split(string(out), "\n") {
			if strings.HasPrefix(strings.TrimSpace(line), "pid = ") {
				pid, _ := strconv.Atoi(strings.TrimSpace(strings.SplitN(line, "=", 2)[1]))
				return pid
			}
		}
		return 0
	}
	out, err = m.run(ctx, "--user", "show", "--property=MainPID", "--value", Unit)
	if err != nil {
		return 0
	}
	pid, _ := strconv.Atoi(strings.TrimSpace(string(out)))
	return pid
}

func (m manager) start(ctx context.Context) error {
	all, err := m.connectorProcesses()
	if err != nil {
		return err
	}
	for _, p := range all {
		if p.PID != m.pid(ctx) {
			return errors.New("another connector is running; stop it before starting the service")
		}
	}
	if m.mac {
		if _, err := m.run(ctx, "enable", m.target); err != nil {
			return err
		}
		if !m.loaded(ctx) {
			_, err := m.run(ctx, "bootstrap", m.domain, m.path)
			return err
		}
		_, err := m.run(ctx, "kickstart", m.target)
		return err
	}
	if _, err := m.run(ctx, "--user", "daemon-reload"); err != nil {
		return err
	}
	_, err = m.run(ctx, "--user", "enable", "--now", Unit)
	return err
}

func (m manager) stop(ctx context.Context, disable bool) error {
	if m.mac {
		if disable {
			if _, err := m.run(ctx, "disable", m.target); err != nil {
				return err
			}
		}
		if m.loaded(ctx) {
			_, err := m.run(ctx, "bootout", m.target)
			return err
		}
		return nil
	}
	if disable {
		_, err := m.run(ctx, "--user", "disable", "--now", Unit)
		return err
	}
	_, err := m.run(ctx, "--user", "stop", Unit)
	return err
}

func (m manager) checkInstalled() error {
	if _, err := os.Lstat(m.path); errors.Is(err, os.ErrNotExist) {
		return errors.New("Home service is not installed; run hmux-web service install")
	}
	if err := trustedDirectory(filepath.Dir(m.path), false); err != nil {
		return err
	}
	raw, err := readOwned(m.path, 64<<10, true)
	if err != nil {
		return err
	}
	marker := []byte("Description=HMux Home connector")
	if m.mac {
		marker = []byte("<string>" + Label + "</string>")
	}
	if !bytes.Contains(raw, marker) {
		return errors.New("refusing to manage an unrecognized service file")
	}
	return nil
}

var serviceEnvKeys = []string{"PATH", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "TMUX_TMPDIR", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"}

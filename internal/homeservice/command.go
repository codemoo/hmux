package homeservice

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/config"
)

// Run exposes native service management without adding a resident supervisor.
func Run(ctx context.Context, args []string, out io.Writer) error {
	if len(args) == 0 {
		return errors.New(usage)
	}
	action := args[0]
	switch action {
	case "install", "status", "start", "stop", "restart", "uninstall":
	default:
		return errors.New(usage)
	}
	f := flag.NewFlagSet("service "+action, flag.ContinueOnError)
	f.SetOutput(out)
	endpoint := f.String("url", "", "Home gateway wss://host/connect")
	token := f.String("token-file", "", "private connector token path")
	configPath := f.String("config", "", "existing Home configuration")
	adopt := f.Bool("from-running", false, "adopt the sole running Home connector and gracefully stop that exact process")
	binaryTarget := f.String("binary", "", "stable service binary destination (default ~/.local/bin/hmux-web)")
	if err := f.Parse(args[1:]); err != nil {
		return err
	}
	if f.NArg() != 0 {
		return errors.New(usage)
	}
	if action != "install" && (*endpoint != "" || *token != "" || *configPath != "" || *adopt || *binaryTarget != "") {
		return errors.New("connection options are only accepted for service install")
	}
	if *adopt && (*endpoint != "" || *token != "" || *configPath != "") {
		return errors.New("--from-running cannot be combined with connection options")
	}
	m, err := newManager()
	if err != nil {
		return err
	}
	if err := m.available(ctx); err != nil {
		return err
	}
	if action != "status" {
		if err := trustedDirectory(filepath.Dir(m.path), true); err != nil {
			return err
		}
		lock, err := lockFile(m.path + ".lock")
		if err != nil {
			return fmt.Errorf("another service change may be in progress: %w", err)
		}
		defer lock.Close()
	}
	if action == "install" {
		return m.install(ctx, *endpoint, *token, *configPath, *binaryTarget, *adopt, out)
	}
	if err := m.checkInstalled(); err != nil {
		return err
	}
	switch action {
	case "status":
		if m.mac {
			raw, err := m.run(ctx, "print", m.target)
			if err != nil {
				fmt.Fprintln(out, "Home service is installed but stopped.")
				return nil
			}
			for _, line := range strings.Split(string(raw), "\n") {
				trimmed := strings.TrimSpace(line)
				if strings.HasPrefix(trimmed, "state = ") || strings.HasPrefix(trimmed, "pid = ") || strings.HasPrefix(trimmed, "last exit code = ") {
					fmt.Fprintln(out, trimmed)
				}
			}
		} else {
			raw, err := m.run(ctx, "--user", "show", Unit, "--property=ActiveState,SubState,MainPID,ExecMainStatus,Result")
			if err != nil {
				return err
			}
			fmt.Fprint(out, string(raw))
		}
		fmt.Fprintln(out, "Process state does not confirm gateway connectivity. Check the service log and web UI.")
	case "start":
		return m.start(ctx)
	case "stop":
		return m.stop(ctx, true)
	case "restart":
		if err := m.stop(ctx, false); err != nil {
			return err
		}
		if err := m.waitForStop(ctx); err != nil {
			return err
		}
		return m.start(ctx)
	case "uninstall":
		if err := m.stop(ctx, true); err != nil {
			return err
		}
		if _, err := config.Backup(m.path, time.Now()); err != nil {
			return err
		}
		if err := os.Remove(m.path); err != nil {
			return err
		}
		if !m.mac {
			if _, err := m.run(ctx, "--user", "daemon-reload"); err != nil {
				return err
			}
		}
		fmt.Fprintln(out, "Home service removed. Native binaries, credentials, workspaces and tmux sessions are retained.")
	}
	return nil
}

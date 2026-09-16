package client

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/codemoo/hmux/archive/terminal/frame"
	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/sshconfig"
	"github.com/codemoo/hmux/internal/tabstate"
	"github.com/codemoo/hmux/internal/workflow"
)

func remoteSSHBaseArgs(alias string) []string {
	return []string{
		"-o", "BatchMode=yes",
		"-o", "ForwardAgent=no",
		"-o", "ClearAllForwardings=yes",
		alias, "--",
	}
}

func Catalog(ctx context.Context, cfg config.ClientConfig) (model.Catalog, error) {
	launcherID := os.Getenv("HMUX_LAUNCHER_ID")
	if launcherID != "" {
		if err := tabstate.ValidateLauncherID(launcherID); err != nil {
			return model.Catalog{}, err
		}
	}
	if cfg.Role == "home" {
		value, err := agent.CatalogAt(ctx, cfg.StateDir)
		if err != nil {
			return model.Catalog{}, err
		}
		if launcherID != "" {
			tabs, tabsErr := (tabstate.Store{StateDir: cfg.StateDir}).Tabs(launcherID)
			if tabsErr != nil && !errors.Is(tabsErr, os.ErrNotExist) {
				return model.Catalog{}, tabsErr
			}
			if tabsErr == nil {
				value.OpenTabs = tabs.Sessions
				value.CurrentTabID = tabs.CurrentID
			}
		}
		return value, nil
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return model.Catalog{}, errors.New("unsafe home_alias or agent_path")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "catalog")
	if launcherID != "" {
		args = append(args, "--launcher", launcherID)
	}
	// #nosec G702 -- the SSH alias and remote executable are strict ASCII
	// allowlists, launcherID is validated, and exec.Command never invokes a
	// local shell. OpenSSH receives each local argument separately.
	cmd := exec.CommandContext(ctx, "ssh", args...)
	output, err := safeexec.Output(cmd, 32*1024*1024)
	if err != nil {
		return model.Catalog{}, fmt.Errorf("remote catalog: %w", err)
	}
	if err := model.ValidateCatalogJSONStructure(output); err != nil {
		return model.Catalog{}, fmt.Errorf("decode remote catalog structure: %w", err)
	}
	var value model.Catalog
	decoder := json.NewDecoder(bytes.NewReader(output))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&value); err != nil {
		return value, fmt.Errorf("decode remote catalog: %w", err)
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return value, errors.New("decode remote catalog: trailing JSON")
	}
	return normalizeRemoteCatalog(value, cfg, launcherID)
}

func normalizeRemoteCatalog(value model.Catalog, cfg config.ClientConfig, launcherID string) (model.Catalog, error) {
	if value.ProtocolVersion != model.ProtocolVersion {
		return value, fmt.Errorf("agent protocol mismatch: client=%d agent=%d", model.ProtocolVersion, value.ProtocolVersion)
	}
	if err := model.ValidateHostMetrics(value.HostMetrics); err != nil {
		value.HostMetrics = nil
	}
	for index := range value.Sessions {
		if err := model.ValidateSessionID(value.Sessions[index].ID); err != nil {
			return value, fmt.Errorf("remote catalog: %w", err)
		}
		if prior := value.Sessions[index].RestoredFrom; prior != nil {
			if model.ValidateSessionID(prior.ID) != nil || prior.CreatedAt < 1 {
				return value, errors.New("remote catalog: invalid recovery identity")
			}
		}
		if err := workflow.ValidateSessionPayload(value.Sessions[index]); err != nil {
			return value, fmt.Errorf("remote catalog: %w", err)
		}
		value.Sessions[index].HostAlias = cfg.HomeAlias
	}
	if err := validateCatalogTabs(value, launcherID); err != nil {
		return model.Catalog{}, err
	}
	return value, nil
}

func validateCatalogTabs(value model.Catalog, launcherID string) error {
	if launcherID == "" {
		if len(value.OpenTabs) != 0 || value.CurrentTabID != "" {
			return errors.New("remote catalog returned unsolicited launcher tabs")
		}
		return nil
	}
	seen := make(map[string]struct{}, len(value.OpenTabs))
	for _, id := range value.OpenTabs {
		if err := model.ValidateSessionID(id); err != nil {
			return fmt.Errorf("remote launcher tabs: %w", err)
		}
		if _, exists := seen[id]; exists {
			return errors.New("remote launcher tabs contain a duplicate session")
		}
		seen[id] = struct{}{}
	}
	if len(value.OpenTabs) > 256 {
		return errors.New("remote launcher tab count exceeds limit")
	}
	if len(value.OpenTabs) == 0 {
		if value.CurrentTabID != "" {
			return errors.New("remote empty launcher tabs have a current session")
		}
		return nil
	}
	if err := model.ValidateSessionID(value.CurrentTabID); err != nil {
		return errors.New("remote launcher tabs have no valid current session")
	}
	if _, exists := seen[value.CurrentTabID]; !exists {
		return errors.New("remote launcher current session is not an open tab")
	}
	return nil
}

func SyncConfig(ctx context.Context, cfg config.ClientConfig, dryRun bool) error {
	if !safeAlias(cfg.DMZAlias) || !safeRemotePath(cfg.ControlPath) {
		return errors.New("unsafe dmz_alias or control_path")
	}
	inventoryData, err := FetchRendered(ctx, cfg, "inventory")
	if err != nil {
		return err
	}
	tempInventory, err := os.CreateTemp("", "hmux-inventory-*.toml")
	if err != nil {
		return err
	}
	tempInventoryPath := tempInventory.Name()
	defer os.Remove(tempInventoryPath)
	if _, err := tempInventory.Write(inventoryData); err != nil {
		_ = tempInventory.Close()
		return err
	}
	if err := tempInventory.Close(); err != nil {
		return err
	}
	inventory, err := config.LoadInventory(tempInventoryPath)
	if err != nil {
		return err
	}
	sshData, err := FetchRendered(ctx, cfg, "ssh")
	if err != nil {
		return err
	}
	expectedSSH, err := sshconfig.Render(inventory)
	if err != nil {
		return err
	}
	if string(sshData) != string(expectedSSH) {
		return errors.New("DMZ rendered SSH fragment does not match canonical inventory")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	sshDir := filepath.Join(home, ".ssh", "config.d")
	if err := ensurePrivateDirectory(filepath.Join(home, ".ssh")); err != nil {
		return err
	}
	if err := ensurePrivateDirectory(sshDir); err != nil {
		return err
	}
	tempSSH, err := os.CreateTemp(sshDir, ".hmux-ssh-*.conf")
	if err != nil {
		return err
	}
	tempSSHPath := tempSSH.Name()
	defer os.Remove(tempSSHPath)
	if err := tempSSH.Chmod(0o600); err != nil {
		_ = tempSSH.Close()
		return err
	}
	if _, err := tempSSH.Write(sshData); err != nil {
		_ = tempSSH.Close()
		return err
	}
	if err := tempSSH.Close(); err != nil {
		return err
	}
	for _, alias := range []string{cfg.DMZAlias, cfg.HomeAlias} {
		if output, err := exec.CommandContext(ctx, "ssh", "-G", "-F", tempSSHPath, alias).CombinedOutput(); err != nil {
			return fmt.Errorf("validate SSH alias %s: %w: %s", alias, err, model.SafeText(string(output), 500))
		}
	}
	if dryRun {
		return nil
	}
	inventorySnapshot, err := snapshotManaged(cfg.InventoryPath)
	if err != nil {
		return err
	}
	sshPath := filepath.Join(sshDir, "50-hmux.generated.conf")
	sshSnapshot, err := snapshotManaged(sshPath)
	if err != nil {
		return err
	}
	if err := config.AtomicWrite(cfg.InventoryPath, inventoryData, 0o600); err != nil {
		return err
	}
	if err := config.AtomicWrite(sshPath, sshData, 0o600); err != nil {
		if restoreErr := inventorySnapshot.restore(); restoreErr != nil {
			return fmt.Errorf("write SSH fragment failed: %v; inventory restore failed: %w", err, restoreErr)
		}
		_ = sshSnapshot.restore()
		return err
	}
	if err := validateInstalledSSH(ctx, inventory); err != nil {
		inventoryRestoreErr := inventorySnapshot.restore()
		sshRestoreErr := sshSnapshot.restore()
		if inventoryRestoreErr != nil || sshRestoreErr != nil {
			return fmt.Errorf("installed SSH validation failed: %v; restore errors: inventory=%v ssh=%v", err, inventoryRestoreErr, sshRestoreErr)
		}
		return err
	}
	return nil
}

func FetchRendered(ctx context.Context, cfg config.ClientConfig, kind string) ([]byte, error) {
	if !safeAlias(cfg.DMZAlias) || !safeRemotePath(cfg.ControlPath) {
		return nil, errors.New("unsafe dmz_alias or control_path")
	}
	switch kind {
	case "inventory", "ssh", "termius":
	default:
		return nil, errors.New("rendered kind must be inventory, ssh or termius")
	}
	args := append(remoteSSHBaseArgs(cfg.DMZAlias), cfg.ControlPath, "rendered", kind)
	command := exec.CommandContext(ctx, "ssh", args...)
	output, err := safeexec.Output(command, 16*1024*1024)
	if err != nil {
		return nil, fmt.Errorf("fetch rendered %s: %w", kind, err)
	}
	if len(output) == 0 || len(output) > 16*1024*1024 {
		return nil, fmt.Errorf("rendered %s has invalid size", kind)
	}
	return output, nil
}

func validateInstalledSSH(ctx context.Context, inventory model.Inventory) error {
	aliases := make(map[string]string, len(inventory.Hosts))
	for _, host := range inventory.Hosts {
		aliases[host.ID] = host.SSHAlias
	}
	for _, host := range inventory.Hosts {
		output, err := safeexec.Output(exec.CommandContext(ctx, "ssh", "-G", host.SSHAlias), 1024*1024)
		if err != nil {
			return fmt.Errorf("validate installed SSH alias %s: %w", host.SSHAlias, err)
		}
		values := parseSSHValues(output)
		expectedJump := ""
		if host.ProxyJump != "" {
			expectedJump = aliases[host.ProxyJump]
		}
		checks := map[string]string{
			"hostname":              host.Address,
			"user":                  host.User,
			"port":                  fmt.Sprint(host.Port),
			"forwardagent":          "no",
			"identitiesonly":        "yes",
			"stricthostkeychecking": "ask",
			"proxyjump":             expectedJump,
		}
		for key, expected := range checks {
			if values[key] != expected {
				return fmt.Errorf("installed SSH alias %s has unexpected %s", host.SSHAlias, key)
			}
		}
	}
	return nil
}

func parseSSHValues(data []byte) map[string]string {
	result := map[string]string{}
	for _, line := range strings.Split(string(data), "\n") {
		fields := strings.Fields(line)
		if len(fields) >= 2 && result[fields[0]] == "" {
			result[fields[0]] = fields[1]
		}
	}
	return result
}

func Attach(cfg config.ClientConfig, id string, shared bool) error {
	return attach(cfg, id, 0, shared)
}

func AttachExpected(cfg config.ClientConfig, id string, createdAt int64, shared bool) error {
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	return attach(cfg, id, createdAt, shared)
}

// AttachExpectedAppView is the native app's terminal attach path. It creates
// one temporary grouped tmux session so the app can hide tmux's status line
// without mutating any pre-existing session option. The view is killed after
// the terminal exits; killing a grouped view never kills its shared windows or
// the target session.
func AttachExpectedAppView(cfg config.ClientConfig, id string, createdAt int64, shared bool) error {
	if err := model.ValidateSessionID(id); err != nil {
		return err
	}
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	if cfg.Role == "home" {
		viewName, err := newAppViewName()
		if err != nil {
			return err
		}
		return attachLocalAppView(id, createdAt, viewName, shared)
	}
	return attachRemoteAppView(cfg, id, createdAt, shared)
}

func newAppViewName() (string, error) {
	var entropy [12]byte
	if _, err := rand.Read(entropy[:]); err != nil {
		return "", fmt.Errorf("generate app view identity: %w", err)
	}
	return fmt.Sprintf("hmux-app-view-%d-%x", os.Getpid(), entropy[:]), nil
}

func attachLocalAppView(id string, createdAt int64, viewName string, shared bool) error {
	runner := catalog.TmuxRunner{}
	setupCtx, setupCancel := context.WithTimeout(context.Background(), 15*time.Second)
	err := createExpectedAppView(setupCtx, runner, id, createdAt, viewName)
	setupCancel()
	if err != nil {
		return err
	}
	defer cleanupAppView(runner, viewName)
	args, err := catalog.AppViewAttachArgs(viewName, shared)
	if err != nil {
		return err
	}
	path, err := catalog.TmuxPath()
	if err != nil {
		return err
	}
	command := exec.Command(path, args...)
	command.Env = os.Environ()
	command.Stdin = os.Stdin
	command.Stdout = os.Stdout
	command.Stderr = os.Stderr
	return command.Run()
}

func attachRemoteAppView(cfg config.ClientConfig, id string, createdAt int64, shared bool) error {
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	if !remoteAgentSupportsCapability(cfg, "native-app-view-v1") {
		return errors.New("remote hmux-agent does not support native app views; update hmux-agent on the Home Mac")
	}
	sshPath, err := exec.LookPath("ssh")
	if err != nil {
		return err
	}
	args := append([]string{"-tt"}, remoteSSHBaseArgs(cfg.HomeAlias)...)
	args = append(args, cfg.AgentPath,
		"attach", "--app-view", "--created-at", strconv.FormatInt(createdAt, 10),
	)
	if shared {
		args = append(args, "--shared")
	}
	args = append(args, remoteSessionArg(id))
	// #nosec G702 -- sshPath comes from LookPath; the SSH alias and remote
	// executable use strict allowlists, the session ID is validated and shell-
	// protected, and every other argument is fixed or a validated integer.
	command := exec.Command(sshPath, args...)
	command.Env = os.Environ()
	command.Stdin = os.Stdin
	command.Stdout = os.Stdout
	command.Stderr = os.Stderr
	if err := command.Run(); err != nil {
		return fmt.Errorf("remote native app attach: %w", err)
	}
	return nil
}

func createExpectedAppView(ctx context.Context, runner catalog.Runner, id string, createdAt int64, viewName string) error {
	if err := requireTmuxCreatedAt(ctx, runner, id, createdAt); err != nil {
		return err
	}
	createArgs, err := catalog.AppViewCreateArgs(id, viewName)
	if err != nil {
		return err
	}
	if _, err := runner.Output(ctx, createArgs...); err != nil {
		cleanupAppView(runner, viewName)
		return fmt.Errorf("create app tmux view: %w", err)
	}
	if err := requireTmuxCreatedAt(ctx, runner, id, createdAt); err != nil {
		cleanupAppView(runner, viewName)
		return err
	}
	return nil
}

func requireTmuxCreatedAt(ctx context.Context, runner catalog.Runner, id string, expected int64) error {
	output, err := runner.Output(ctx, "display-message", "-p", "-t", id, "#{session_created}")
	if err != nil {
		return fmt.Errorf("verify expected tmux session: %w", err)
	}
	actual, err := strconv.ParseInt(strings.TrimSpace(string(output)), 10, 64)
	if err != nil || actual != expected {
		return catalog.ErrSessionChanged
	}
	return nil
}

func cleanupAppView(runner catalog.Runner, viewName string) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	args, err := catalog.AppViewKillArgs(viewName)
	if err == nil {
		_, _ = runner.Output(ctx, args...)
	}
}

func attach(cfg config.ClientConfig, id string, createdAt int64, shared bool) error {
	if err := model.ValidateSessionID(id); err != nil {
		return err
	}
	remoteID := remoteSessionArg(id)
	launcherID := os.Getenv("HMUX_LAUNCHER_ID")
	if launcherID != "" {
		if err := tabstate.ValidateLauncherID(launcherID); err != nil {
			return err
		}
	}
	if cfg.Role == "home" {
		if createdAt > 0 {
			if launcherID != "" {
				return errors.New("expected attach does not support framed launchers")
			}
			return catalog.ExecAttachExpected(id, createdAt, shared)
		}
		if launcherID != "" {
			frameConfig, frameUIConfig, err := frame.DefaultConfigPaths()
			if err != nil {
				return err
			}
			return frame.ExecAttach(frame.Options{
				StateDir: cfg.StateDir, ConfigPath: frameConfig,
				UIConfigPath: frameUIConfig, LauncherID: launcherID,
				SessionID: id, Client: true,
			})
		}
		return catalog.ExecAttach(id, shared)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	sshPath, err := exec.LookPath("ssh")
	if err != nil {
		return err
	}
	args := append([]string{"ssh", "-tt"}, remoteSSHBaseArgs(cfg.HomeAlias)...)
	args = append(args, cfg.AgentPath, "attach")
	if shared {
		args = append(args, "--shared")
	}
	if createdAt > 0 {
		if !remoteAgentSupportsCapability(cfg, "expected-identity-v1") {
			return errors.New("remote hmux-agent does not support safe expected-identity attach; update hmux-agent on the Home Mac")
		}
		args = append(args, "--created-at", strconv.FormatInt(createdAt, 10))
	}
	if launcherID != "" {
		args = append(args, "--launcher", launcherID)
	}
	args = append(args, remoteID)
	if launcherID != "" {
		// A launcher must survive the framed SSH child so it can redraw the
		// local selector in the same process. Replacing it with ssh forced the
		// shell entrypoint to start a new hmux process and exposed a blank
		// terminal between the remote frame and fzf.
		// #nosec G204,G702 -- sshPath comes from LookPath and every dynamic
		// argument is independently validated before this argument-array call.
		command := exec.Command(sshPath, args[1:]...)
		command.Env = os.Environ()
		command.Stdin = os.Stdin
		command.Stdout = os.Stdout
		command.Stderr = os.Stderr
		if err := command.Run(); err != nil {
			var exit *exec.ExitError
			if errors.As(err, &exit) && exit.ExitCode() == 130 {
				return frame.ErrLauncherExit
			}
			return fmt.Errorf("remote framed attach: %w", err)
		}
		return nil
	}
	// #nosec G702 -- sshPath comes from LookPath; every dynamic argument is
	// validated by safeAlias, safeRemotePath, ValidateLauncherID, or
	// ValidateSessionID, and syscall.Exec never invokes a shell.
	return syscall.Exec(sshPath, args, os.Environ())
}

func CleanupLauncher(ctx context.Context, cfg config.ClientConfig, launcherID string) error {
	if err := tabstate.ValidateLauncherID(launcherID); err != nil {
		return err
	}
	if cfg.Role == "home" {
		return (tabstate.Store{StateDir: cfg.StateDir}).Cleanup(launcherID)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "launcher-cleanup", launcherID)
	command := exec.CommandContext(ctx, "ssh", args...)
	if output, err := command.CombinedOutput(); err != nil {
		return fmt.Errorf("remote launcher cleanup: %w: %s", err, model.SafeText(string(output), 500))
	}
	return nil
}

func SetAlias(ctx context.Context, cfg config.ClientConfig, sessionID, alias string) error {
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if cfg.Role == "home" {
		return agent.SetAlias(ctx, cfg.StateDir, sessionID, alias)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "alias-set", remoteSessionArg(sessionID))
	command := exec.CommandContext(ctx, "ssh", args...)
	command.Stdin = strings.NewReader(alias + "\n")
	if _, err := safeexec.Output(command, 64*1024); err != nil {
		return fmt.Errorf("remote alias update: %w", err)
	}
	return nil
}

func SetAliasExpected(ctx context.Context, cfg config.ClientConfig, sessionID string, createdAt int64, alias string) error {
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	if cfg.Role == "home" {
		return agent.SetAliasExpected(ctx, cfg.StateDir, sessionID, createdAt, alias)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	capabilities, capabilityErr := remoteAgentCapabilities(ctx, cfg)
	if capabilityErr != nil && ctx.Err() != nil {
		return ctx.Err()
	}
	if capabilityErr != nil || !hasCapability(capabilities, "expected-identity-v1") {
		return errors.New("remote hmux-agent does not support safe expected-identity alias updates; update hmux-agent on the Home Mac")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "alias-set", "--created-at", strconv.FormatInt(createdAt, 10), remoteSessionArg(sessionID))
	command := exec.CommandContext(ctx, "ssh", args...)
	command.Stdin = strings.NewReader(alias + "\n")
	if _, err := safeexec.Output(command, 64*1024); err != nil {
		if remoteSessionChanged(err) {
			return catalog.ErrSessionChanged
		}
		return fmt.Errorf("remote expected alias update: %w", err)
	}
	return nil
}

func SetHiddenExpected(ctx context.Context, cfg config.ClientConfig, sessionID string, createdAt int64, hidden bool) error {
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	if cfg.Role == "home" {
		return agent.SetHiddenExpected(ctx, cfg.StateDir, sessionID, createdAt, hidden)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	capabilities, capabilityErr := remoteAgentCapabilities(ctx, cfg)
	if capabilityErr != nil && ctx.Err() != nil {
		return ctx.Err()
	}
	if capabilityErr != nil || !hasCapability(capabilities, "expected-identity-v1") ||
		!hasCapability(capabilities, "session-visibility-v1") {
		return errors.New("remote hmux-agent does not support safe expected-identity session visibility updates; update hmux-agent on the Home Mac")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "hidden-set", "--created-at", strconv.FormatInt(createdAt, 10), remoteSessionArg(sessionID))
	command := exec.CommandContext(ctx, "ssh", args...)
	command.Stdin = strings.NewReader(strconv.FormatBool(hidden) + "\n")
	if _, err := safeexec.Output(command, 64*1024); err != nil {
		if remoteSessionChanged(err) {
			return catalog.ErrSessionChanged
		}
		return fmt.Errorf("remote expected session visibility update: %w", err)
	}
	return nil
}

func TerminateSession(ctx context.Context, cfg config.ClientConfig, sessionID string) error {
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if cfg.Role == "home" {
		return catalog.TerminateSession(ctx, catalog.TmuxRunner{}, sessionID)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "terminate", "--confirmed", remoteSessionArg(sessionID))
	command := exec.CommandContext(ctx, "ssh", args...)
	if _, err := safeexec.Output(command, 64*1024); err != nil {
		return fmt.Errorf("remote session termination: %w", err)
	}
	return nil
}

func TerminateSessionExpected(ctx context.Context, cfg config.ClientConfig, sessionID string, createdAt int64) error {
	if err := model.ValidateSessionID(sessionID); err != nil {
		return err
	}
	if createdAt < 1 {
		return errors.New("invalid session creation time")
	}
	if cfg.Role == "home" {
		return catalog.TerminateSessionExpected(ctx, catalog.TmuxRunner{}, sessionID, createdAt)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	capabilities, capabilityErr := remoteAgentCapabilities(ctx, cfg)
	if capabilityErr != nil && ctx.Err() != nil {
		return ctx.Err()
	}
	if capabilityErr != nil || !hasCapability(capabilities, "expected-identity-v1") {
		return errors.New("remote hmux-agent does not support safe expected-identity termination; update hmux-agent on the Home Mac")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "terminate", "--confirmed", "--created-at", strconv.FormatInt(createdAt, 10), remoteSessionArg(sessionID))
	command := exec.CommandContext(ctx, "ssh", args...)
	if _, err := safeexec.Output(command, 64*1024); err != nil {
		if remoteSessionChanged(err) {
			return catalog.ErrSessionChanged
		}
		return fmt.Errorf("remote expected session termination: %w", err)
	}
	return nil
}

func remoteSessionChanged(err error) bool {
	return strings.TrimSpace(safeexec.Stderr(err)) == "hmux-agent: "+catalog.ErrSessionChanged.Error()
}

func remoteAgentSupportsCapability(cfg config.ClientConfig, capability string) bool {
	if len(capability) < 1 || len(capability) > 128 || strings.ContainsAny(capability, " \t\r\n") {
		return false
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	capabilities, err := remoteAgentCapabilities(ctx, cfg)
	return err == nil && hasCapability(capabilities, capability)
}

func remoteAgentCapabilities(ctx context.Context, cfg config.ClientConfig) (map[string]struct{}, error) {
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return nil, errors.New("unsafe home_alias or agent_path")
	}
	probeCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "capabilities")
	command := exec.CommandContext(probeCtx, "ssh", args...)
	output, err := safeexec.Output(command, 4096)
	if err != nil {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		return nil, err
	}
	return parseCapabilities(output)
}

func parseCapabilities(output []byte) (map[string]struct{}, error) {
	capabilities := make(map[string]struct{})
	for _, line := range strings.Split(strings.TrimSuffix(string(output), "\n"), "\n") {
		if !stableID(line) {
			return nil, errors.New("remote hmux-agent returned invalid capabilities")
		}
		capabilities[line] = struct{}{}
	}
	return capabilities, nil
}

func hasCapability(capabilities map[string]struct{}, capability string) bool {
	_, ok := capabilities[capability]
	return ok
}

// UpdateAgent asks only a self-update-capable Home agent to perform its own
// signed, role-bound update. Older agents are left untouched for the explicit
// timestamped bootstrap path; the client never uploads or executes an
// unverified replacement over SSH.
func UpdateAgent(ctx context.Context, cfg config.ClientConfig) (bool, error) {
	if cfg.Role == "home" {
		path, err := exec.LookPath("hmux-agent")
		if err != nil {
			return false, err
		}
		output, err := safeexec.Output(exec.CommandContext(ctx, path, "capabilities"), 4096)
		if err != nil || !capabilityOutputContains(output, "signed-self-update-v1") {
			return false, nil
		}
		if _, err := safeexec.Output(exec.CommandContext(ctx, path, "update", "--if-newer"), 64*1024); err != nil {
			return true, fmt.Errorf("local agent update: %w", err)
		}
		return true, nil
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return false, errors.New("unsafe home_alias or agent_path")
	}
	capabilities, capabilityErr := remoteAgentCapabilities(ctx, cfg)
	if capabilityErr != nil && ctx.Err() != nil {
		return false, ctx.Err()
	}
	if capabilityErr != nil || !hasCapability(capabilities, "signed-self-update-v1") {
		return false, nil
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "update", "--if-newer")
	command := exec.CommandContext(ctx, "ssh", args...)
	if _, err := safeexec.Output(command, 64*1024); err != nil {
		return true, fmt.Errorf("remote agent update: %w", err)
	}
	return true, nil
}

func capabilityOutputContains(output []byte, capability string) bool {
	for _, line := range strings.Split(string(output), "\n") {
		if strings.TrimSpace(line) == capability {
			return true
		}
	}
	return false
}

func Create(ctx context.Context, cfg config.ClientConfig, inventory model.Inventory, profileID, name string) (string, error) {
	if cfg.Role == "home" {
		return agent.Create(ctx, inventory, profileID, name, cfg.StateDir)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return "", errors.New("unsafe home_alias or agent_path")
	}
	if !stableID(profileID) || (name != "" && !safeSessionName(name)) {
		return "", errors.New("invalid profile or session name")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "create")
	if name != "" {
		args = append(args, "--name-stdin")
	}
	args = append(args, profileID)
	command := exec.CommandContext(ctx, "ssh", args...)
	if name != "" {
		command.Stdin = strings.NewReader(name + "\n")
	}
	output, err := safeexec.Output(command, 4096)
	if err != nil {
		return "", fmt.Errorf("remote create: %w", err)
	}
	id := strings.TrimSpace(string(output))
	if err := model.ValidateSessionID(id); err != nil {
		return "", err
	}
	return id, nil
}

func CreateSession(ctx context.Context, cfg config.ClientConfig, inventory model.Inventory, profileID, name string) (agent.CreateResult, error) {
	if cfg.Role == "home" {
		return agent.CreateSession(ctx, inventory, profileID, name, cfg.StateDir)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return agent.CreateResult{}, errors.New("unsafe home_alias or agent_path")
	}
	if !stableID(profileID) || (name != "" && !safeSessionName(name)) {
		return agent.CreateResult{}, errors.New("invalid profile or session name")
	}
	capabilities, err := remoteAgentCapabilities(ctx, cfg)
	if err != nil && ctx.Err() != nil {
		return agent.CreateResult{}, ctx.Err()
	}
	if err != nil || !hasCapability(capabilities, "structured-create-v1") {
		return agent.CreateResult{}, errors.New("remote hmux-agent does not support authoritative session creation; update hmux-agent on the Home Mac")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "create", "--json")
	if name != "" {
		args = append(args, "--name-stdin")
	}
	args = append(args, profileID)
	command := exec.CommandContext(ctx, "ssh", args...)
	if name != "" {
		command.Stdin = strings.NewReader(name + "\n")
	}
	output, err := safeexec.Output(command, 4096)
	if err != nil {
		return agent.CreateResult{}, fmt.Errorf("remote create: %w", err)
	}
	var wireResult struct {
		ID        string `json:"id"`
		CreatedAt int64  `json:"created_at"`
		Reused    *bool  `json:"reused"`
	}
	decoder := json.NewDecoder(bytes.NewReader(output))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&wireResult); err != nil {
		return agent.CreateResult{}, fmt.Errorf("decode remote create: %w", err)
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return agent.CreateResult{}, errors.New("decode remote create: trailing JSON")
	}
	if err := model.ValidateSessionID(wireResult.ID); err != nil {
		return agent.CreateResult{}, fmt.Errorf("remote create identity: %w", err)
	}
	if wireResult.CreatedAt < 1 {
		return agent.CreateResult{}, errors.New("remote create identity has invalid creation time")
	}
	if wireResult.Reused == nil {
		return agent.CreateResult{}, errors.New("remote create identity is missing reuse state")
	}
	return agent.CreateResult{
		ID: wireResult.ID, CreatedAt: wireResult.CreatedAt, Reused: *wireResult.Reused,
	}, nil
}

func ValidateCreate(ctx context.Context, cfg config.ClientConfig, inventory model.Inventory, profileID, name string) (string, error) {
	if cfg.Role == "home" {
		return agent.ValidateCreate(inventory, profileID, name)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return "", errors.New("unsafe home_alias or agent_path")
	}
	if !stableID(profileID) || (name != "" && !safeSessionName(name)) {
		return "", errors.New("invalid profile or session name")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "create", "--dry-run")
	if name != "" {
		args = append(args, "--name-stdin")
	}
	args = append(args, profileID)
	command := exec.CommandContext(ctx, "ssh", args...)
	if name != "" {
		command.Stdin = strings.NewReader(name + "\n")
	}
	output, err := safeexec.Output(command, 4096)
	if err != nil {
		return "", fmt.Errorf("remote create dry-run: %w", err)
	}
	planned := strings.TrimSpace(string(output))
	if planned == "" || len(planned) > 80 || strings.ContainsAny(planned, "\r\n") {
		return "", errors.New("remote create dry-run returned an invalid session name")
	}
	return planned, nil
}

func ResolveSession(value model.Catalog, input string) (string, error) {
	if model.ValidateSessionID(input) == nil {
		for _, session := range value.Sessions {
			if session.ID == input {
				return input, nil
			}
		}
		return "", fmt.Errorf("session %s does not exist", input)
	}
	var matches []string
	for _, session := range value.Sessions {
		if session.Name == input || (session.Alias != "" && session.Alias == input) {
			matches = append(matches, session.ID)
		}
	}
	if len(matches) == 1 {
		return matches[0], nil
	}
	if len(matches) == 0 {
		return "", fmt.Errorf("session %q does not exist", input)
	}
	return "", fmt.Errorf("session name %q is ambiguous", input)
}

func WriteLast(cfg config.ClientConfig, id string) error {
	if err := model.ValidateSessionID(id); err != nil {
		return err
	}
	return config.AtomicWrite(filepath.Join(cfg.StateDir, "last-session"), []byte(id+"\n"), 0o600)
}

func ReadLast(cfg config.ClientConfig) (string, error) {
	data, err := os.ReadFile(filepath.Join(cfg.StateDir, "last-session"))
	if err != nil {
		return "", err
	}
	id := strings.TrimSpace(string(data))
	if err := model.ValidateSessionID(id); err != nil {
		return "", err
	}
	return id, nil
}

func safeAlias(value string) bool {
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

func safeRemotePath(value string) bool {
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

// OpenSSH joins remote command arguments into a command interpreted by the
// remote login shell. tmux stable IDs begin with '$', so passing one verbatim
// makes the shell expand it as a positional parameter (for example, "$7")
// before hmux-agent starts. Validate first at every caller, then escape only
// that fixed leading metacharacter. The remote agent receives the original ID
// and older protocol-compatible agents remain supported.
func remoteSessionArg(id string) string {
	return `\` + id
}

func stableID(value string) bool {
	if len(value) < 1 || len(value) > 63 || value[0] < 'a' || value[0] > 'z' {
		return false
	}
	for _, r := range value {
		if !((r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') || r == '-') {
			return false
		}
	}
	return true
}

func safeSessionName(value string) bool {
	count := utf8.RuneCountInString(value)
	if count < 1 || count > 80 {
		return false
	}
	for _, r := range value {
		if unicode.IsLetter(r) || unicode.IsNumber(r) || r == ' ' || r == '_' || r == '-' {
			continue
		}
		return false
	}
	return true
}

func ensurePrivateDirectory(path string) error {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return err
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.IsDir() {
		return fmt.Errorf("%s must be a real directory, not a symlink", filepath.Base(path))
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Getuid() {
		return fmt.Errorf("%s must be owned by the current user", filepath.Base(path))
	}
	return os.Chmod(path, 0o700)
}

type managedSnapshot struct {
	path       string
	backupPath string
	existed    bool
}

func snapshotManaged(path string) (managedSnapshot, error) {
	result := managedSnapshot{path: path}
	if _, err := os.Stat(path); errors.Is(err, os.ErrNotExist) {
		return result, nil
	} else if err != nil {
		return result, err
	}
	backup, err := config.Backup(path, time.Now())
	if err != nil {
		return result, err
	}
	result.existed = true
	result.backupPath = backup
	return result, nil
}

func (s managedSnapshot) restore() error {
	if !s.existed {
		return os.Remove(s.path)
	}
	data, err := os.ReadFile(s.backupPath)
	if err != nil {
		return err
	}
	info, err := os.Stat(s.backupPath)
	if err != nil {
		return err
	}
	return config.AtomicWrite(s.path, data, info.Mode().Perm())
}

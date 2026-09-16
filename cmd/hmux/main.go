package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/csv"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/archive/terminal/frame"
	"github.com/codemoo/hmux/archive/terminal/ui"
	"github.com/codemoo/hmux/internal/client"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/release"
	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/workflow"
	"golang.org/x/term"
)

var version = "dev"
var autoUpdateCheck = autoUpdate
var appRuntimeBrokerRun = appRuntimeBroker
var foregroundAppCatalogStreamRun = runForegroundAppCatalogStream
var foregroundAppUsageStreamRun = runForegroundAppUsageStream
var fileStageParentPollInterval = 2 * time.Second

func main() {
	if err := run(os.Args[1:]); err != nil {
		var appExit appCommandExitError
		if errors.As(err, &appExit) {
			os.Exit(1)
		}
		if os.Getenv("HMUX_LAUNCHER") == "1" {
			ui.RestoreLauncherScreen()
		}
		if errors.Is(err, frame.ErrLauncherExit) {
			os.Exit(130)
		}
		if errors.Is(err, ui.ErrCancelled) {
			if os.Getenv("HMUX_LAUNCHER") == "1" {
				os.Exit(130)
			}
			return
		}
		fmt.Fprintln(os.Stderr, "hmux:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	global := flag.NewFlagSet("hmux", flag.ContinueOnError)
	configPath := global.String("config", "", "client config path")
	shared := global.Bool("shared", false, "allow shared tmux attach")
	noUpdate := global.Bool("no-update-check", false, "skip update check")
	verbose := global.Bool("verbose", false, "verbose diagnostics")
	debug := global.Bool("debug", false, "debug diagnostics (sensitive values remain masked)")
	exact := global.Bool("exact", false, "skip selector for an exact unique session match")
	selectorLines := global.Bool("selector-lines", false, "internal selector reload output")
	selectorSort := global.String("selector-sort", "session", "internal selector sort column")
	selectorDirection := global.String("selector-direction", "asc", "internal selector sort direction")
	selectorQuery := global.String("selector-query", "", "internal selector metadata query")
	selectorFooter := global.Bool("selector-footer", false, "internal selector footer output")
	selectorWidth := global.Int("selector-width", 100, "internal selector width")
	if err := global.Parse(args); err != nil {
		return err
	}
	cfg, err := config.LoadClient(*configPath)
	if err != nil {
		return err
	}
	args = global.Args()
	// The foreground stream is an app-owned transport helper. It must start
	// without update brokerage or network work so bootstrap output is bounded
	// and cannot recursively spawn another runtime helper.
	if len(args) == 2 && args[0] == "app" && args[1] == "catalog-stream" {
		return foregroundAppCatalogStreamRun(cfg, os.Stdout)
	}
	if len(args) == 2 && args[0] == "app" && args[1] == "usage-stream" {
		return foregroundAppUsageStreamRun(cfg, os.Stdout)
	}
	if cfg.UpdateCheck && !*noUpdate && len(args) > 0 && args[0] == "app" &&
		!(len(args) == 2 && (args[1] == "rollback-native" || args[1] == "file-stage")) {
		if err := appRuntimeBrokerRun(cfg, os.Args[1:], *verbose || *debug); err != nil && (*verbose || *debug) {
			fmt.Fprintln(os.Stderr, "app update check:", err)
		}
	}
	if *selectorFooter {
		help := "↵ attach   / search   ^N new   ^X terminate   ^R alias   ^Q exit"
		fmt.Print(ui.SelectorFooter(help, max(32, *selectorWidth-8)))
		return nil
	}
	timeout := time.Duration(cfg.Timeout) * time.Second
	if cfg.UpdateCheck && !*noUpdate && shouldAutoUpdate(args) {
		updateCtx, updateCancel := context.WithTimeout(context.Background(), timeout)
		updated, updateErr := autoUpdateCheck(updateCtx, cfg)
		updateCancel()
		if updateErr != nil {
			if *verbose || *debug {
				fmt.Fprintln(os.Stderr, "update check:", updateErr)
			}
		} else if updated {
			return execCurrent(cfg, os.Args[1:])
		}
	}
	// A slow or unavailable update check must not consume the timeout budget
	// for the command the user actually requested.
	commandTimeout := timeout
	fileStageCommand := len(args) == 2 && args[0] == "app" && args[1] == "file-stage"
	if fileStageCommand {
		commandTimeout = 5 * time.Minute
	}
	timeoutCtx, timeoutCancel := context.WithTimeout(context.Background(), commandTimeout)
	defer timeoutCancel()
	ctx := context.Context(timeoutCtx)
	if len(args) == 2 && args[0] == "app" && args[1] == "conversation" {
		var stopConversation context.CancelFunc
		ctx, stopConversation = signal.NotifyContext(timeoutCtx, os.Interrupt, syscall.SIGHUP, syscall.SIGTERM)
		defer stopConversation()
	}
	stop := func() {}
	if fileStageCommand {
		var contextErr error
		ctx, stop, contextErr = fileStageAppContext(timeoutCtx, os.Getppid)
		if contextErr != nil {
			return contextErr
		}
		defer stop()
	}
	if *selectorLines {
		value, err := client.Catalog(ctx, cfg)
		if err != nil {
			return err
		}
		sessions, err := ui.SortSessions(ui.SelectableSessions(value.Sessions), *selectorSort, *selectorDirection)
		if err != nil {
			return err
		}
		sessions = ui.FilterSessions(sessions, *selectorQuery)
		fmt.Print(ui.SelectorLinesAtWidth(sessions, false, *selectorWidth))
		return nil
	}
	if len(args) == 0 {
		return selectAndAttach(ctx, cfg, resolvedConfigPath(*configPath), "", *shared, *exact)
	}
	switch args[0] {
	case "app":
		return runApp(ctx, cfg, args[1:], os.Stdin, os.Stdout, os.Getenv)
	case "ls":
		return list(ctx, cfg, args[1:])
	case "attach":
		if len(args) != 2 {
			return errors.New("usage: hmux attach <session-id-or-name>")
		}
		value, err := client.Catalog(ctx, cfg)
		if err != nil {
			return err
		}
		id, err := client.ResolveSession(value, args[1])
		if err != nil {
			return err
		}
		_ = client.WriteLast(cfg, id)
		return client.Attach(cfg, id, *shared)
	case "last":
		id, err := client.ReadLast(cfg)
		if err != nil {
			return fmt.Errorf("no valid last session: %w", err)
		}
		return client.Attach(cfg, id, *shared)
	case "frame-host":
		if len(args) != 1 {
			return errors.New("usage: hmux frame-host")
		}
		return frame.RunHost()
	case "frame-ui":
		if len(args) != 1 {
			return errors.New("usage: hmux frame-ui")
		}
		return frame.RunUI()
	case "frame-inner":
		if len(args) != 1 {
			return errors.New("usage: hmux frame-inner")
		}
		return frame.RunInner(cfg.StateDir)
	case "frame-spacer":
		if len(args) != 1 {
			return errors.New("usage: hmux frame-spacer")
		}
		return frame.RunSpacer()
	case "launcher-cleanup":
		if len(args) != 2 {
			return errors.New("usage: hmux launcher-cleanup <launcher-id>")
		}
		return client.CleanupLauncher(ctx, cfg, args[1])
	case "new":
		if len(args) == 3 && args[1] == "--inline" {
			return newSessionInline(ctx, cfg, args[2])
		}
		return newSession(ctx, cfg, args[1:], *shared)
	case "alias":
		if len(args) == 4 && args[1] == "--inline" {
			return client.SetAlias(ctx, cfg, args[2], args[3])
		}
		if len(args) != 2 {
			return errors.New("usage: hmux alias [--inline] <stable-session-id> [alias]")
		}
		return setSessionAlias(ctx, cfg, args[1:])
	case "terminate":
		if len(args) == 4 && args[1] == "--inline" {
			return terminateSessionInline(ctx, cfg, args[2], args[3])
		}
		if len(args) != 2 {
			return errors.New("usage: hmux terminate [--inline] <stable-session-id> [confirmation]")
		}
		return terminateSession(ctx, cfg, args[1:])
	case "sync", "update":
		dryRun := len(args) == 2 && args[1] == "--dry-run"
		if len(args) > 1 && !dryRun {
			return fmt.Errorf("usage: hmux %s [--dry-run]", args[0])
		}
		if args[0] == "sync" {
			if err := client.SyncConfig(ctx, cfg, dryRun); err != nil {
				return err
			}
			if dryRun {
				fmt.Println("dry-run: inventory and SSH fragment validated; no files changed")
			} else {
				fmt.Println("inventory and SSH fragment synchronized")
			}
		}
		return update(ctx, cfg, *verbose || *debug, dryRun)
	case "rollback":
		return rollback(cfg, args[1:])
	case "doctor":
		return doctor(ctx, cfg, args[1:])
	case "version":
		return versions(ctx, cfg, args[1:])
	case "host":
		return host(ctx, cfg, args[1:])
	case "termius-sync":
		return termiusSync(ctx, cfg, args[1:])
	case "workflow":
		return showWorkflow(ctx, cfg, args[1:])
	case "--preview-session":
		return errors.New("preview is available inside the selector")
	default:
		query := strings.Join(args, " ")
		if *noUpdate {
			cfg.UpdateCheck = false
		}
		return selectAndAttach(ctx, cfg, resolvedConfigPath(*configPath), query, *shared, *exact)
	}
}

func versions(ctx context.Context, cfg config.ClientConfig, args []string) error {
	jsonOutput := len(args) == 1 && args[0] == "--json"
	if len(args) > 0 && !jsonOutput {
		return errors.New("usage: hmux version [--json]")
	}
	result := map[string]any{
		"ok": true,
		"client": map[string]any{
			"version": version, "protocol": model.ProtocolVersion, "platform": release.Platform(),
		},
	}
	run := func(name string, command *exec.Cmd) {
		output, err := safeexec.Output(command, 64*1024)
		if err != nil {
			result[name] = map[string]any{"ok": false, "error": "version query failed"}
			result["ok"] = false
			return
		}
		result[name] = map[string]any{"ok": true, "value": model.SafeText(string(output), 500)}
	}
	if cfg.Role == "home" {
		if path, err := exec.LookPath("hmux-agent"); err == nil {
			run("agent", exec.CommandContext(ctx, path, "version"))
		} else {
			result["agent"] = map[string]any{"ok": false, "error": "hmux-agent not installed"}
			result["ok"] = false
		}
	} else {
		run("agent", exec.CommandContext(ctx, "ssh", batchSSHArgs(cfg.HomeAlias, cfg.AgentPath, "version")...))
	}
	run("control", exec.CommandContext(ctx, "ssh", batchSSHArgs(cfg.DMZAlias, cfg.ControlPath, "version")...))
	if jsonOutput {
		data, _ := json.MarshalIndent(result, "", "  ")
		fmt.Println(string(data))
	} else {
		clientVersion := result["client"].(map[string]any)
		fmt.Printf("hmux %s protocol=%d platform=%s\n", clientVersion["version"], clientVersion["protocol"], clientVersion["platform"])
		for _, name := range []string{"agent", "control"} {
			entry := result[name].(map[string]any)
			if value, present := entry["value"]; present {
				fmt.Printf("%s: %s\n", name, value)
			} else {
				fmt.Printf("%s: unavailable\n", name)
			}
		}
	}
	if healthy, _ := result["ok"].(bool); !healthy {
		return errors.New("one or more version queries failed")
	}
	return nil
}

func shouldAutoUpdate(args []string) bool {
	if len(args) == 0 {
		return true
	}
	switch args[0] {
	case "sync", "update", "rollback", "doctor", "version", "host", "termius-sync", "workflow", "app", "launcher-cleanup", "alias", "terminate", "frame-host", "frame-ui", "frame-inner", "frame-spacer":
		return false
	default:
		return true
	}
}

func showWorkflow(ctx context.Context, cfg config.ClientConfig, args []string) error {
	filter, jsonOutput, err := parseWorkflowArgs(args)
	if err != nil {
		return err
	}
	value, err := client.Catalog(ctx, cfg)
	if err != nil {
		return err
	}
	views, err := workflow.Views(value.Sessions, filter)
	if err != nil {
		return err
	}
	if jsonOutput {
		result := struct {
			ProtocolVersion int                    `json:"protocol_version"`
			GeneratedAt     time.Time              `json:"generated_at"`
			Sessions        []workflow.SessionView `json:"sessions"`
		}{model.ProtocolVersion, value.GeneratedAt, views}
		data, err := json.MarshalIndent(result, "", "  ")
		if err != nil {
			return err
		}
		fmt.Println(string(data))
		return nil
	}
	fmt.Print(workflow.FormatViews(views))
	return nil
}

func parseWorkflowArgs(args []string) (filter string, jsonOutput bool, err error) {
	for _, arg := range args {
		if arg == "--json" {
			if jsonOutput {
				return "", false, errors.New("usage: hmux workflow [session] [--json]")
			}
			jsonOutput = true
			continue
		}
		if strings.HasPrefix(arg, "-") || filter != "" {
			return "", false, errors.New("usage: hmux workflow [session] [--json]")
		}
		filter = arg
	}
	return filter, jsonOutput, nil
}

func autoUpdate(ctx context.Context, cfg config.ClientConfig) (bool, error) {
	manifest, err := release.FetchManifest(ctx, cfg)
	if err != nil {
		return false, err
	}
	baseline := version
	if selected, selectedErr := release.SelectedVersion(cfg); selectedErr == nil && release.IsNewerVersion(selected, baseline) {
		baseline = selected
	}
	if !release.IsNewerVersion(manifest.Version, baseline) {
		return false, nil
	}
	artifact, err := release.FetchArtifact(ctx, cfg, manifest)
	if err != nil {
		return false, err
	}
	_, err = release.Install(cfg, manifest, artifact)
	return err == nil, err
}

func autoUpdateForApp(ctx context.Context, cfg config.ClientConfig) (bool, error) {
	manifest, err := release.FetchManifest(ctx, cfg)
	if err != nil {
		return false, err
	}
	baseline := version
	if selected, selectedErr := release.SelectedVersion(cfg); selectedErr == nil && release.IsNewerVersion(selected, baseline) {
		baseline = selected
	}
	if !release.IsNewerVersion(manifest.Version, baseline) {
		return false, nil
	}
	artifact, err := release.FetchArtifact(ctx, cfg, manifest)
	if err != nil {
		return false, err
	}
	validator := func(path string) error {
		return release.ValidateAppBackend(ctx, path, release.CurrentAppBackendRequirement())
	}
	_, err = release.InstallValidated(cfg, manifest, artifact, validator)
	return err == nil, err
}

func appRuntimeBroker(cfg config.ClientConfig, args []string, verbose bool) error {
	now := time.Now()
	var checkErr error
	allowSelectedExec := true
	if release.AppUpdateCheckDue(cfg.CacheDir, now, time.Hour) {
		updateCtx, updateCancel := context.WithTimeout(context.Background(), time.Duration(cfg.Timeout)*time.Second)
		updated, updateErr := autoUpdateForApp(updateCtx, cfg)
		updateCancel()
		agentCtx, agentCancel := context.WithTimeout(context.Background(), time.Duration(cfg.Timeout)*time.Second)
		_, agentUpdateErr := client.UpdateAgent(agentCtx, cfg)
		agentCancel()
		recordErr := release.RecordAppUpdateCheck(cfg.CacheDir, now)
		checkErr = errors.Join(updateErr, agentUpdateErr, recordErr)
		allowSelectedExec = shouldExecuteSelectedAppBackend(updated, agentUpdateErr)
		if verbose && updated {
			fmt.Fprintln(os.Stderr, "verified HMux runtime update installed")
		}
	}
	selected, path, selectedErr := release.SelectedExecutable(cfg)
	if allowSelectedExec && selectedErr == nil && release.IsNewerVersion(selected, version) {
		validationCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		validationErr := release.ValidateAppBackend(validationCtx, path, release.CurrentAppBackendRequirement())
		cancel()
		if validationErr == nil {
			return execReleasePath(path, args)
		}
		checkErr = errors.Join(checkErr, validationErr)
	} else if selectedErr != nil && !errors.Is(selectedErr, os.ErrNotExist) {
		checkErr = errors.Join(checkErr, selectedErr)
	}
	return checkErr
}

func shouldExecuteSelectedAppBackend(installedThisInvocation bool, agentUpdateErr error) bool {
	return !installedThisInvocation || agentUpdateErr == nil
}

func execCurrent(cfg config.ClientConfig, args []string) error {
	_, path, err := release.SelectedExecutable(cfg)
	if err != nil {
		return err
	}
	return execReleasePath(path, args)
}

func execReleasePath(path string, args []string) error {
	argv := []string{path, "--no-update-check"}
	argv = append(argv, args...)
	// #nosec G204,G702 -- path is fixed beneath the validated cache root and
	// argv is passed directly to execve, never through a shell.
	return syscall.Exec(path, argv, os.Environ())
}

func list(ctx context.Context, cfg config.ClientConfig, args []string) error {
	jsonOutput := len(args) == 1 && args[0] == "--json"
	if len(args) > 0 && !jsonOutput {
		return errors.New("usage: hmux ls [--json]")
	}
	value, err := client.Catalog(ctx, cfg)
	if err != nil {
		return err
	}
	if jsonOutput {
		data, _ := json.MarshalIndent(value, "", "  ")
		fmt.Println(string(data))
		return nil
	}
	for _, session := range value.Sessions {
		fmt.Println(ui.FormatRow(session, false, terminalWidth()))
	}
	return nil
}

func selectAndAttach(ctx context.Context, cfg config.ClientConfig, selectedConfigPath, query string, shared, exact bool) error {
	launcher := os.Getenv("HMUX_LAUNCHER") == "1"
	resume := false
	catalogCtx := ctx
	var catalogCancel context.CancelFunc
	for {
		value, err := client.Catalog(catalogCtx, cfg)
		if catalogCancel != nil {
			catalogCancel()
			catalogCancel = nil
		}
		if err != nil {
			return err
		}
		if exact && query != "" {
			var match string
			for _, session := range ui.SelectableSessions(value.Sessions) {
				if session.ID == query || session.Name == query || (session.Alias != "" && session.Alias == query) {
					if match != "" {
						match = ""
						break
					}
					match = session.ID
				}
			}
			if match != "" {
				_ = client.WriteLast(cfg, match)
				return client.Attach(cfg, match, shared)
			}
		}
		executable, _ := os.Executable()
		openTabs := resolveOpenTabs(value)
		selector := ui.Selector{
			Query: query, Resume: resume, Executable: executable,
			OpenTabs: openTabs, CurrentTabID: value.CurrentTabID,
			RefreshArgs:   []string{"--config", selectedConfigPath, "--selector-lines", "--no-update-check"},
			FooterArgs:    []string{"--config", selectedConfigPath, "--selector-footer", "--no-update-check"},
			NewArgs:       []string{"--config", selectedConfigPath, "--no-update-check", "new"},
			TerminateArgs: []string{"--config", selectedConfigPath, "--no-update-check", "terminate"},
			AliasArgs:     []string{"--config", selectedConfigPath, "--no-update-check", "alias"},
		}
		id, err := selector.Select(catalogCtx, ui.SelectableSessions(value.Sessions))
		if err != nil {
			return err
		}
		_ = client.WriteLast(cfg, id)
		if err := client.Attach(cfg, id, shared); err != nil {
			return err
		}
		if !launcher {
			return nil
		}
		// Keep the preserved fzf screen visible while a fresh catalog is
		// collected. A new timeout is required because the original command
		// context may have expired while the user worked inside the session.
		resume = true
		query = ""
		nextCtx, cancel := context.WithTimeout(
			context.WithoutCancel(ctx), time.Duration(cfg.Timeout)*time.Second,
		)
		catalogCtx = nextCtx
		catalogCancel = cancel
	}
}

func resolveOpenTabs(value model.Catalog) []model.Session {
	if len(value.OpenTabs) == 0 {
		return nil
	}
	byID := make(map[string]model.Session, len(value.Sessions))
	for _, session := range value.Sessions {
		byID[session.ID] = session
	}
	result := make([]model.Session, 0, len(value.OpenTabs))
	for _, id := range value.OpenTabs {
		if session, exists := byID[id]; exists {
			result = append(result, session)
		}
	}
	return result
}

func terminateSessionInline(ctx context.Context, cfg config.ClientConfig, id, confirmation string) error {
	switch strings.ToLower(strings.TrimSpace(confirmation)) {
	case "y", "yes":
	default:
		return errors.New("inline termination requires yes")
	}
	return client.TerminateSession(ctx, cfg, id)
}

func setSessionAlias(ctx context.Context, cfg config.ClientConfig, args []string) error {
	session, err := sessionByID(ctx, cfg, args[0])
	if err != nil {
		return err
	}
	prompt := fmt.Sprintf(
		"Session: %s (%s)\nCurrent alias: %s\nNew alias (empty = original name): ",
		model.SafeText(session.Name, 512), session.ID, emptyPromptValue(session.Alias),
	)
	alias, err := readTTYLine(prompt, 512)
	if err != nil {
		return err
	}
	actionCtx, cancel := context.WithTimeout(context.Background(), time.Duration(cfg.Timeout)*time.Second)
	defer cancel()
	if err := client.SetAliasExpected(actionCtx, cfg, session.ID, session.CreatedAt, alias); err != nil {
		return err
	}
	if strings.TrimSpace(alias) == "" {
		fmt.Fprintf(os.Stderr, "Alias cleared; displaying original session name %q.\n", model.SafeText(session.Name, 512))
	} else {
		fmt.Fprintf(os.Stderr, "Alias set to %q; tmux session name remains %q.\n",
			model.SafeText(strings.TrimSpace(alias), 128), model.SafeText(session.Name, 512))
	}
	return nil
}

func terminateSession(ctx context.Context, cfg config.ClientConfig, args []string) error {
	session, err := sessionByID(ctx, cfg, args[0])
	if err != nil {
		return err
	}
	name := session.Name
	if strings.TrimSpace(session.Alias) != "" {
		name = session.Alias
	}
	answer, err := readTTYLine(fmt.Sprintf(
		"Terminate %q (%s)?\nThis stops every process in the tmux session. Type yes to continue: ",
		model.SafeText(name, 512), session.ID,
	), 16)
	if err != nil {
		return err
	}
	switch strings.ToLower(strings.TrimSpace(answer)) {
	case "y", "yes":
	default:
		fmt.Fprintln(os.Stderr, "Termination cancelled.")
		return nil
	}
	actionCtx, cancel := context.WithTimeout(context.Background(), time.Duration(cfg.Timeout)*time.Second)
	defer cancel()
	if err := client.TerminateSessionExpected(actionCtx, cfg, session.ID, session.CreatedAt); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "Terminated tmux session %q (%s).\n", model.SafeText(name, 512), session.ID)
	return nil
}

func sessionByID(ctx context.Context, cfg config.ClientConfig, id string) (model.Session, error) {
	if err := model.ValidateSessionID(id); err != nil {
		return model.Session{}, err
	}
	value, err := client.Catalog(ctx, cfg)
	if err != nil {
		return model.Session{}, err
	}
	for _, session := range value.Sessions {
		if session.ID == id {
			return session, nil
		}
	}
	return model.Session{}, fmt.Errorf("session %s does not exist", id)
}

func readTTYLine(prompt string, maxBytes int64) (string, error) {
	if maxBytes < 1 {
		return "", errors.New("invalid input limit")
	}
	tty, err := os.OpenFile("/dev/tty", os.O_RDWR, 0)
	if err != nil {
		return "", errors.New("interactive terminal is unavailable")
	}
	defer tty.Close()
	if _, err := fmt.Fprint(tty, prompt); err != nil {
		return "", err
	}
	reader := bufio.NewReader(io.LimitReader(tty, maxBytes+2))
	line, readErr := reader.ReadString('\n')
	if readErr != nil && !errors.Is(readErr, io.EOF) {
		return "", readErr
	}
	line = strings.TrimSuffix(strings.TrimSuffix(line, "\n"), "\r")
	if int64(len(line)) > maxBytes {
		return "", fmt.Errorf("input exceeds %d bytes", maxBytes)
	}
	return line, nil
}

func emptyPromptValue(value string) string {
	value = strings.TrimSpace(model.SafeText(value, 128))
	if value == "" {
		return "(none)"
	}
	return value
}

func newSession(ctx context.Context, cfg config.ClientConfig, args []string, shared bool) error {
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil {
		return err
	}
	profileID := ""
	name := ""
	dryRun := false
	for index := 0; index < len(args); index++ {
		switch args[index] {
		case "--name":
			if index+1 >= len(args) {
				return errors.New("--name requires a value")
			}
			name = args[index+1]
			index++
		case "--dry-run":
			dryRun = true
		default:
			if strings.HasPrefix(args[index], "-") || profileID != "" {
				return errors.New("usage: hmux new [profile] [--name name] [--dry-run]")
			}
			profileID = args[index]
		}
	}
	if profileID == "" {
		profiles := append([]model.Profile(nil), inventory.Profiles...)
		profileID, err = ui.SelectProfile(ctx, profiles)
		if err != nil {
			return err
		}
	}
	if dryRun {
		planned, err := client.ValidateCreate(ctx, cfg, inventory, profileID, name)
		if err != nil {
			return err
		}
		fmt.Printf("dry-run: profile %s validated; would create or reuse session %q\n", profileID, planned)
		return nil
	}
	id, err := client.Create(ctx, cfg, inventory, profileID, name)
	if err != nil {
		return err
	}
	_ = client.WriteLast(cfg, id)
	return client.Attach(cfg, id, shared)
}

func newSessionInline(ctx context.Context, cfg config.ClientConfig, spec string) error {
	spec = strings.TrimSpace(model.SafeText(spec, 512))
	fields := strings.Fields(spec)
	if len(fields) == 0 {
		return errors.New("inline create requires: profile [session name]")
	}
	profileID := fields[0]
	name := strings.TrimSpace(strings.TrimPrefix(spec, profileID))
	inventory, err := config.LoadInventory(cfg.InventoryPath)
	if err != nil {
		return err
	}
	_, err = client.Create(ctx, cfg, inventory, profileID, name)
	return err
}

func update(ctx context.Context, cfg config.ClientConfig, verbose, dryRun bool) error {
	manifest, err := release.FetchManifest(ctx, cfg)
	if err != nil {
		if _, selectedErr := release.SelectedVersion(cfg); selectedErr == nil {
			fmt.Fprintf(os.Stderr, "DMZ unavailable; keeping last-known-good client: %v\n", err)
			return nil
		}
		return err
	}
	if selected, selectedErr := release.SelectedVersion(cfg); selectedErr == nil {
		if selected == manifest.Version {
			fmt.Printf("hmux %s is already selected\n", selected)
			return nil
		}
		if !release.IsNewerVersion(manifest.Version, selected) {
			return fmt.Errorf("refusing release rollback from %s to %s; use hmux rollback", selected, manifest.Version)
		}
	} else if !errors.Is(selectedErr, os.ErrNotExist) {
		return fmt.Errorf("verify current release before update: %w", selectedErr)
	}
	artifact, err := release.FetchArtifact(ctx, cfg, manifest)
	if err != nil {
		return err
	}
	if dryRun {
		fmt.Printf("dry-run: signed hmux %s verified (%d bytes); cache unchanged\n", manifest.Version, len(artifact))
		return nil
	}
	path, err := release.Install(cfg, manifest, artifact)
	if err != nil {
		return err
	}
	if verbose {
		fmt.Printf("installed %s (%d bytes, sha256 verified)\n", path, manifest.Size)
	} else {
		fmt.Printf("hmux %s installed and selected\n", manifest.Version)
	}
	return nil
}

func rollback(cfg config.ClientConfig, args []string) error {
	releasesDir := filepath.Join(cfg.CacheDir, "releases")
	dryRun := false
	version := ""
	for _, arg := range args {
		if arg == "--dry-run" {
			dryRun = true
		} else if version == "" {
			version = arg
		} else {
			version = ""
			break
		}
	}
	if version == "" {
		entries, _ := os.ReadDir(releasesDir)
		fmt.Println("available cached releases:")
		for _, entry := range entries {
			if entry.IsDir() {
				fmt.Println(" ", entry.Name())
			}
		}
		return errors.New("usage: hmux rollback [--dry-run] <version>")
	}
	if dryRun {
		if err := release.VerifyCached(cfg, version); err != nil {
			return err
		}
		fmt.Printf("dry-run: cached release %s signature verified; current unchanged\n", version)
		return nil
	}
	if err := release.Rollback(cfg, version); err != nil {
		return err
	}
	fmt.Printf("rolled back to %s\n", version)
	return nil
}

func doctor(ctx context.Context, cfg config.ClientConfig, args []string) error {
	jsonOutput := len(args) == 1 && args[0] == "--json"
	if len(args) > 0 && !jsonOutput {
		return errors.New("usage: hmux doctor [--json]")
	}
	ok := true
	result := map[string]any{
		"schema_version": model.SchemaVersion, "version": version,
		"client_id": cfg.ClientID, "role": cfg.Role, "platform": runtime.GOOS + "-" + runtime.GOARCH,
		"launchd_used": false,
	}
	toolPaths := make(map[string]string)
	checkTool := func(name string) {
		path, err := doctorToolPath(name)
		if err != nil {
			result[name] = map[string]any{"ok": false, "error": "not installed"}
			ok = false
			return
		}
		toolPaths[name] = path
		result[name] = map[string]any{"ok": true, "path": path}
	}
	for _, name := range []string{"ssh", "tmux", "fzf", "ghostty"} {
		checkTool(name)
	}
	if value, err := client.Catalog(ctx, cfg); err == nil {
		result["catalog"] = map[string]any{"ok": true, "sessions": len(value.Sessions), "protocol": value.ProtocolVersion}
	} else {
		result["catalog"] = map[string]any{"ok": false, "error": err.Error()}
		ok = false
	}
	if cfg.Role == "remote" {
		for _, alias := range []string{cfg.DMZAlias, cfg.HomeAlias} {
			output, err := safeexec.Output(exec.CommandContext(ctx, "ssh", "-G", alias), 1024*1024)
			entry := map[string]any{"ok": err == nil}
			if err == nil {
				text := strings.ToLower(string(output))
				entry["forward_agent_no"] = strings.Contains(text, "forwardagent no")
				entry["identities_only_yes"] = strings.Contains(text, "identitiesonly yes")
				entry["proxy_jump_configured"] = alias != cfg.HomeAlias ||
					strings.Contains(text, "proxyjump "+strings.ToLower(cfg.DMZAlias))
				entry["host_key_checking_enabled"] = !strings.Contains(text, "stricthostkeychecking no") &&
					!strings.Contains(text, "userknownhostsfile /dev/null")
				for _, field := range []string{"forward_agent_no", "identities_only_yes", "proxy_jump_configured", "host_key_checking_enabled"} {
					if value, _ := entry[field].(bool); !value {
						entry["ok"] = false
					}
				}
			}
			result["ssh_"+alias] = entry
			if healthy, _ := entry["ok"].(bool); !healthy {
				ok = false
			}
			connection := exec.CommandContext(ctx, "ssh", batchSSHArgs(alias, "true")...)
			if err := connection.Run(); err != nil {
				result["connect_"+alias] = map[string]any{"ok": false, "error": "batch connection failed"}
				ok = false
			} else {
				result["connect_"+alias] = map[string]any{"ok": true}
			}
		}
		if safeControlPath(cfg.ControlPath) {
			output, err := safeexec.Output(
				exec.CommandContext(ctx, "ssh", batchSSHArgs(cfg.DMZAlias, cfg.ControlPath, "health")...),
				1024*1024,
			)
			var health map[string]any
			if decodeErr := json.Unmarshal(output, &health); err != nil || decodeErr != nil {
				result["dmz_health"] = map[string]any{"ok": false, "error": "DMZ health unavailable"}
				ok = false
			} else {
				result["dmz_health"] = health
				if healthy, _ := health["ok"].(bool); !healthy {
					ok = false
				}
			}
		}
	}
	if _, err := os.Stat(cfg.PublicKeyPath); err == nil {
		result["release_public_key"] = "pinned"
	} else {
		result["release_public_key"] = "missing (signed updates will be rejected)"
		ok = false
	}
	if inventory, err := config.LoadInventory(cfg.InventoryPath); err == nil {
		result["inventory"] = map[string]any{"ok": true, "revision": inventory.Revision, "hosts": len(inventory.Hosts), "profiles": len(inventory.Profiles)}
	} else {
		result["inventory"] = map[string]any{"ok": false, "error": err.Error()}
		ok = false
	}
	var cached []string
	if entries, err := os.ReadDir(filepath.Join(cfg.CacheDir, "releases")); err == nil {
		for _, entry := range entries {
			if entry.IsDir() && release.ValidVersion(entry.Name()) {
				cached = append(cached, entry.Name())
			}
		}
	}
	result["cached_releases"] = cached
	if data, err := os.ReadFile(filepath.Join(cfg.StateDir, "termius-sync.json")); err == nil {
		var status any
		if json.Unmarshal(data, &status) == nil {
			result["termius_adapter"] = status
		}
	} else {
		result["termius_adapter"] = map[string]any{"mode": "generated-import-artifact", "last_prepared": "never", "automatic_vault_sync": false}
	}
	if ghosttyPath := toolPaths["ghostty"]; ghosttyPath != "" {
		output, fontErr := safeexec.Output(exec.CommandContext(ctx, ghosttyPath, "+list-fonts"), 32*1024*1024)
		fontOK := fontErr == nil && strings.Contains(string(output), "Monatendard Nerd Font Mono")
		result["monatendard_font"] = map[string]any{"ok": fontOK}
		if !fontOK {
			ok = false
		}
	}
	if infocmp, err := exec.LookPath("infocmp"); err == nil {
		err = exec.CommandContext(ctx, infocmp, "xterm-ghostty").Run()
		result["xterm_ghostty_terminfo"] = map[string]any{"ok": err == nil}
		if err != nil {
			ok = false
		}
	}
	result["truecolor_env"] = strings.EqualFold(os.Getenv("COLORTERM"), "truecolor") ||
		strings.EqualFold(os.Getenv("COLORTERM"), "24bit")
	result["ok"] = ok
	data, _ := json.MarshalIndent(result, "", "  ")
	if jsonOutput {
		fmt.Println(string(data))
	} else {
		fmt.Println(string(data))
	}
	if !ok {
		return errors.New("one or more diagnostics failed")
	}
	return nil
}

func doctorToolPath(name string) (string, error) {
	if path, err := exec.LookPath(name); err == nil {
		return path, nil
	}
	if name == "ghostty" && runtime.GOOS == "darwin" {
		path := "/Applications/Ghostty.app/Contents/MacOS/ghostty"
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			return path, nil
		}
	}
	return "", exec.ErrNotFound
}

func safeControlPath(value string) bool {
	return value != "" && !strings.ContainsAny(value, " \t\r\n;&|`$(){}[]<>")
}

func batchSSHArgs(alias string, remote ...string) []string {
	args := []string{
		"-o", "BatchMode=yes",
		"-o", "ForwardAgent=no",
		"-o", "ClearAllForwardings=yes",
		alias, "--",
	}
	return append(args, remote...)
}

func host(ctx context.Context, cfg config.ClientConfig, args []string) error {
	if len(args) == 0 {
		return errors.New("usage: hmux host <list|add|edit|diff>")
	}
	if strings.ContainsAny(cfg.ControlPath, " \t\r\n;&|`$(){}[]<>") {
		return errors.New("unsafe control path")
	}
	operation := args[0]
	remoteArgs := append([]string{"ssh"}, batchSSHArgs(cfg.DMZAlias, cfg.ControlPath, "host", operation, "--json")...)
	var input []byte
	switch operation {
	case "list", "diff":
		if len(args) != 1 {
			return fmt.Errorf("usage: hmux host %s", operation)
		}
	case "add", "edit":
		dryRun := false
		inputPath := ""
		for index := 1; index < len(args); index++ {
			switch args[index] {
			case "--dry-run":
				dryRun = true
			case "--file":
				if index+1 >= len(args) {
					return errors.New("--file requires a path")
				}
				inputPath = args[index+1]
				index++
			default:
				return fmt.Errorf("usage: hmux host %s [--dry-run] [--file path]", operation)
			}
		}
		var err error
		if inputPath == "" || inputPath == "-" {
			input, err = io.ReadAll(io.LimitReader(os.Stdin, 1024*1024+1))
		} else {
			input, err = os.ReadFile(inputPath)
		}
		if err != nil {
			return err
		}
		if len(input) == 0 || len(input) > 1024*1024 {
			return errors.New("host JSON input must be between 1 byte and 1 MiB")
		}
		var request struct {
			Host model.Host `json:"host"`
		}
		decoder := json.NewDecoder(bytes.NewReader(input))
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&request); err != nil {
			return fmt.Errorf("decode host JSON: %w", err)
		}
		var trailing any
		if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
			if err == nil {
				return errors.New("decode host JSON: trailing JSON value")
			}
			return fmt.Errorf("decode host JSON: %w", err)
		}
		if dryRun {
			remoteArgs = append(remoteArgs, "--dry-run")
		}
	default:
		return errors.New("usage: hmux host <list|add|edit|diff>")
	}
	cmd := exec.CommandContext(ctx, remoteArgs[0], remoteArgs[1:]...)
	if len(input) > 0 {
		cmd.Stdin = bytes.NewReader(input)
	}
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func termiusSync(ctx context.Context, cfg config.ClientConfig, args []string) error {
	dryRun := len(args) == 1 && args[0] == "--dry-run"
	if len(args) > 0 && !dryRun {
		return errors.New("usage: hmux termius-sync [--dry-run]")
	}
	fmt.Println("Termius adapter mode: generated-import-artifact (official headless Vault API not configured)")
	data, err := client.FetchRendered(ctx, cfg, "termius")
	if err != nil {
		return err
	}
	reader := csv.NewReader(bytes.NewReader(data))
	reader.FieldsPerRecord = 7
	header, err := reader.Read()
	if err != nil {
		return fmt.Errorf("validate Termius CSV: %w", err)
	}
	expectedHeader := []string{"Label", "Address", "Port", "Username", "Group", "Tags", "JumpHost"}
	if strings.Join(header, "\x00") != strings.Join(expectedHeader, "\x00") {
		return errors.New("validate Termius CSV: unexpected header")
	}
	rows := 0
	for {
		record, readErr := reader.Read()
		if errors.Is(readErr, io.EOF) {
			break
		}
		if readErr != nil {
			return fmt.Errorf("validate Termius CSV: %w", readErr)
		}
		rows++
		if rows > 10000 {
			return errors.New("validate Termius CSV: row limit exceeded")
		}
		if len(record[0]) == 0 || len(record[1]) == 0 {
			return errors.New("validate Termius CSV: host label/address is empty")
		}
	}
	sum := sha256.Sum256(data)
	if dryRun {
		fmt.Printf("dry-run: rows=%d artifact_sha256=%s; no application state changed\n", rows, hex.EncodeToString(sum[:8]))
		fmt.Println("Termius database access is intentionally disabled")
		return nil
	}
	generatedDir := filepath.Join(filepath.Dir(cfg.InventoryPath), "generated")
	destination := filepath.Join(generatedDir, "hmux-hosts.csv")
	backup := ""
	if _, statErr := os.Stat(destination); statErr == nil {
		backup, err = config.Backup(destination, time.Now())
		if err != nil {
			return err
		}
	}
	if err := config.AtomicWrite(destination, data, 0o600); err != nil {
		return err
	}
	status := map[string]any{
		"schema_version": model.SchemaVersion, "mode": "generated-import-artifact",
		"prepared_at": time.Now().UTC(), "rows": rows,
		"artifact_sha256_prefix": hex.EncodeToString(sum[:8]), "automatic_vault_sync": false,
	}
	statusData, _ := json.MarshalIndent(status, "", "  ")
	if err := config.AtomicWrite(filepath.Join(cfg.StateDir, "termius-sync.json"), statusData, 0o600); err != nil {
		return err
	}
	if runtime.GOOS == "darwin" {
		if err := exec.CommandContext(ctx, "open", "-a", "Termius").Start(); err != nil {
			return fmt.Errorf("artifact prepared at %s but Termius could not be opened: %w", destination, err)
		}
	}
	fmt.Printf("Termius import artifact prepared: %s\n", destination)
	if backup != "" {
		fmt.Printf("previous generated artifact backup: %s\n", backup)
	}
	return errors.New("supported Termius UI import and Vault sync confirmation are still required; no private app storage was modified")
}

func resolvedConfigPath(path string) string {
	if path != "" {
		return path
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".config", "hmux", "client.toml")
}

func terminalWidth() int {
	for _, file := range []*os.File{os.Stderr, os.Stdout, os.Stdin} {
		width, _, err := term.GetSize(int(file.Fd()))
		if err == nil && width >= 20 && width <= 1000 {
			return width
		}
	}
	if value, err := strconv.Atoi(os.Getenv("COLUMNS")); err == nil && value >= 20 && value <= 1000 {
		return value
	}
	return 100
}

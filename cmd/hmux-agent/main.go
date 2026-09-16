package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/codemoo/hmux/archive/terminal/frame"
	"github.com/codemoo/hmux/archive/terminal/ui"
	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/client"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/release"
	"github.com/codemoo/hmux/internal/tabstate"
	"github.com/codemoo/hmux/internal/workflow"
)

var version = "dev"

func main() {
	if err := run(os.Args[1:]); err != nil {
		if errors.Is(err, frame.ErrLauncherExit) {
			os.Exit(130)
		}
		fmt.Fprintln(os.Stderr, "hmux-agent:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 0 {
		return usage()
	}
	if args[0] == "catalog-stream" {
		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGHUP, syscall.SIGTERM)
		defer stop()
		return runAgentCatalogStream(ctx, args[1:], os.Stdin, os.Stdout)
	}
	if args[0] == "usage-stream" {
		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGHUP, syscall.SIGTERM)
		defer stop()
		return runAgentUsageStream(ctx, args[1:], os.Stdin, os.Stdout)
	}
	if args[0] == "file-stage" {
		signalCtx, stopSignals := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGHUP, syscall.SIGTERM)
		defer stopSignals()
		ctx, cancel := context.WithTimeout(signalCtx, 5*time.Minute)
		defer cancel()
		stdin := os.Stdin
		cancelDone := make(chan struct{})
		go func() {
			select {
			case <-ctx.Done():
				_ = stdin.Close()
			case <-cancelDone:
			}
		}()
		err := runAgentFileStage(ctx, args[1:], stdin, os.Stdout)
		close(cancelDone)
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	switch args[0] {
	case "workspace":
		return runAgentWorkspace(ctx, args[1:], os.Stdin, os.Stdout)
	case "recovery":
		return runAgentRecovery(ctx, args[1:], os.Stdout)
	case "conversation":
		return runAgentConversation(ctx, args[1:], os.Stdout)
	case "workflow-hook":
		workflowHook(os.Stdin, os.Stdout)
		return nil
	case "workflow-report":
		flags := flag.NewFlagSet("workflow-report", flag.ContinueOnError)
		taskID := flags.String("task-id", "", "sanitized detached task identifier")
		status := flags.String("status", "", "detached task lifecycle status")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if flags.NArg() != 0 || *taskID == "" || *status == "" {
			return errors.New("usage: hmux-agent workflow-report --task-id id --status running|completed|failed|interrupted")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		reportCtx, reportCancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer reportCancel()
		binding, err := workflow.ResolveBinding(reportCtx)
		if err != nil {
			return err
		}
		return (workflow.Store{StateDir: cfg.StateDir}).RecordReport(binding, workflow.Report{TaskID: *taskID, Status: *status})
	case "workflow":
		filter, jsonOutput, err := parseWorkflowArgs(args[1:])
		if err != nil {
			return err
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		value, err := agent.CatalogAt(ctx, cfg.StateDir)
		if err != nil {
			return err
		}
		views, err := workflow.Views(value.Sessions, filter)
		if err != nil {
			return err
		}
		if jsonOutput {
			data, err := json.MarshalIndent(struct {
				ProtocolVersion int                    `json:"protocol_version"`
				GeneratedAt     time.Time              `json:"generated_at"`
				Sessions        []workflow.SessionView `json:"sessions"`
			}{model.ProtocolVersion, value.GeneratedAt, views}, "", "  ")
			if err != nil {
				return err
			}
			fmt.Println(string(data))
			return nil
		}
		fmt.Print(workflow.FormatViews(views))
		return nil
	case "catalog":
		launcherID := ""
		if len(args) == 3 && args[1] == "--launcher" {
			launcherID = args[2]
			if err := tabstate.ValidateLauncherID(launcherID); err != nil {
				return err
			}
		} else if len(args) != 1 {
			return errors.New("usage: hmux-agent catalog [--launcher <launcher-id>]")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		value, err := agent.CatalogAt(ctx, cfg.StateDir)
		if err != nil {
			return err
		}
		if launcherID != "" {
			tabs, tabsErr := (tabstate.Store{StateDir: cfg.StateDir}).Tabs(launcherID)
			if tabsErr != nil && !errors.Is(tabsErr, os.ErrNotExist) {
				return tabsErr
			}
			if tabsErr == nil {
				value.OpenTabs = tabs.Sessions
				value.CurrentTabID = tabs.CurrentID
			}
		}
		data, err := json.MarshalIndent(value, "", "  ")
		if err != nil {
			return err
		}
		fmt.Println(string(data))
		return nil
	case "preview":
		if len(args) != 2 {
			return errors.New("usage: hmux-agent preview <stable-session-id>")
		}
		text, err := agent.Preview(ctx, args[1])
		if err != nil {
			return err
		}
		fmt.Print(text)
		return nil
	case "capabilities":
		if len(args) != 1 {
			return errors.New("usage: hmux-agent capabilities")
		}
		fmt.Println("expected-identity-v1")
		fmt.Println("session-visibility-v1")
		fmt.Println("signed-self-update-v1")
		fmt.Println("catalog-stream-v1")
		fmt.Println("host-metrics-v1")
		fmt.Println("conversation-v1")
		fmt.Println("usage-stream-v1")
		fmt.Println("native-app-view-v1")
		fmt.Println("file-stage-v1")
		fmt.Println("structured-create-v1")
		fmt.Println("shared-workspace-v1")
		return nil
	case "update":
		if len(args) > 2 || (len(args) == 2 && args[1] != "--if-newer") {
			return errors.New("usage: hmux-agent update [--if-newer]")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		now := time.Now()
		if len(args) == 2 && !release.AgentUpdateCheckDue(cfg.StateDir, now, time.Hour) {
			fmt.Println("agent update check deferred")
			return nil
		}
		updated, updateErr := release.UpdateInstalledAgent(ctx, cfg, version)
		recordErr := release.RecordAgentUpdateCheck(cfg.StateDir, now)
		if updateErr != nil {
			return updateErr
		}
		if recordErr != nil {
			return recordErr
		}
		if updated {
			fmt.Println("signed agent update installed")
		} else {
			fmt.Println("agent is current")
		}
		return nil
	case "attach":
		id, launcherID, createdAt, shared, detach, appView, err := parseAttach(args[1:])
		if err != nil {
			return err
		}
		if id == "" {
			return errors.New("usage: hmux-agent attach [--shared|--detach-others] <stable-session-id>")
		}
		if shared && detach {
			return errors.New("--shared and --detach-others are mutually exclusive")
		}
		if appView {
			if launcherID != "" || createdAt < 1 || detach {
				return errors.New("--app-view requires --created-at and cannot use --launcher or --detach-others")
			}
			cfg, err := config.LoadClient("")
			if err != nil {
				return err
			}
			if cfg.Role != "home" {
				return errors.New("native app views are available only on the Home Mac")
			}
			return client.AttachExpectedAppView(cfg, id, createdAt, shared)
		}
		if launcherID != "" {
			if createdAt > 0 {
				return errors.New("--created-at cannot be combined with --launcher")
			}
			cfg, err := config.LoadClient("")
			if err != nil {
				return err
			}
			frameConfig, frameUIConfig, err := frame.DefaultConfigPaths()
			if err != nil {
				return err
			}
			return frame.ExecAttach(frame.Options{
				StateDir: cfg.StateDir, ConfigPath: frameConfig,
				UIConfigPath: frameUIConfig, LauncherID: launcherID,
				SessionID: id,
			})
		}
		if createdAt > 0 {
			return catalog.ExecAttachExpected(id, createdAt, shared)
		}
		return catalog.ExecAttach(id, shared)
	case "frame-host":
		if len(args) != 1 {
			return errors.New("usage: hmux-agent frame-host")
		}
		return frame.RunHost()
	case "frame-ui":
		if len(args) != 1 {
			return errors.New("usage: hmux-agent frame-ui")
		}
		return frame.RunUI()
	case "frame-inner":
		if len(args) != 1 {
			return errors.New("usage: hmux-agent frame-inner")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		return frame.RunInner(cfg.StateDir)
	case "frame-spacer":
		if len(args) != 1 {
			return errors.New("usage: hmux-agent frame-spacer")
		}
		return frame.RunSpacer()
	case "frame-status":
		if len(args) != 3 || args[1] != "--launcher" {
			return errors.New("usage: hmux-agent frame-status --launcher <launcher-id>")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		status, err := frame.Status(ctx, cfg.StateDir, args[2])
		if err != nil {
			return err
		}
		fmt.Print(status)
		return nil
	case "frame-click":
		flags := flag.NewFlagSet("frame-click", flag.ContinueOnError)
		launcherID := flags.String("launcher", "", "launcher identifier")
		statusFile := flags.String("status-file", "", "private frame status path")
		mouseRange := flags.String("mouse-range", "", "validated status mouse range")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if flags.NArg() != 0 {
			return errors.New("usage: hmux-agent frame-click --launcher id --status-file path --mouse-range range")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		return frame.Click(ctx, cfg.StateDir, *launcherID, *statusFile, *mouseRange)
	case "metadata-migrate":
		clear := len(args) == 2 && args[1] == "--clear"
		if len(args) > 1 && !clear {
			return errors.New("usage: hmux-agent metadata-migrate [--clear]")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		count, err := agent.MigrateLegacyMetadata(ctx, cfg.StateDir, clear)
		if err != nil {
			return err
		}
		fmt.Printf("migrated metadata for %d sessions; cleared=%t\n", count, clear)
		return nil
	case "launcher-cleanup":
		if len(args) != 2 {
			return errors.New("usage: hmux-agent launcher-cleanup <launcher-id>")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		return (tabstate.Store{StateDir: cfg.StateDir}).Cleanup(args[1])
	case "select":
		flags := flag.NewFlagSet("select", flag.ContinueOnError)
		mobile := flags.Bool("mobile", false, "use narrow mobile layout")
		shared := flags.Bool("shared", false, "allow concurrent clients")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		value, err := agent.CatalogAt(ctx, cfg.StateDir)
		if err != nil {
			return err
		}
		executable, _ := os.Executable()
		refresh := []string{"selector-lines"}
		if *mobile {
			refresh = append(refresh, "--mobile")
		}
		id, err := (ui.Selector{
			Mobile: *mobile, Executable: executable, RefreshArgs: refresh,
			FooterArgs:    []string{"selector-footer"},
			NewArgs:       []string{"create"},
			TerminateArgs: []string{"terminate"}, AliasArgs: []string{"alias"},
		}).Select(ctx, ui.SelectableSessions(value.Sessions))
		if errors.Is(err, ui.ErrCancelled) {
			return nil
		}
		if err != nil {
			return err
		}
		return catalog.ExecAttach(id, *shared)
	case "create":
		if len(args) == 3 && args[1] == "--inline" {
			return createSessionInline(ctx, args[2])
		}
		profile, name, inventoryPath, dryRun, nameStdin, jsonOutput, err := parseCreate(args[1:])
		if err != nil {
			return err
		}
		if nameStdin {
			name, err = readBoundedSingleLine(os.Stdin, 512)
			if err != nil {
				return fmt.Errorf("read session name: %w", err)
			}
		}
		if profile == "" {
			return errors.New("usage: hmux-agent create <profile-id> [--name name|--name-stdin] [--json]")
		}
		inventory, err := config.LoadInventory(inventoryPath)
		if err != nil {
			return err
		}
		if dryRun {
			planned, err := agent.ValidateCreate(inventory, profile, name)
			if err != nil {
				return err
			}
			fmt.Println(planned)
			return nil
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		if jsonOutput {
			result, err := agent.CreateSession(ctx, inventory, profile, name, cfg.StateDir)
			if err != nil {
				return err
			}
			return json.NewEncoder(os.Stdout).Encode(result)
		}
		id, err := agent.Create(ctx, inventory, profile, name, cfg.StateDir)
		if err != nil {
			return err
		}
		fmt.Println(id)
		return nil
	case "doctor":
		return doctor(ctx)
	case "selector-lines":
		flags := flag.NewFlagSet("selector-lines", flag.ContinueOnError)
		mobile := flags.Bool("mobile", false, "use narrow mobile layout")
		sortColumn := flags.String("selector-sort", "session", "selector sort column")
		sortDirection := flags.String("selector-direction", "asc", "selector sort direction")
		selectorQuery := flags.String("selector-query", "", "selector metadata query")
		selectorWidth := flags.Int("selector-width", 100, "selector screen width")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if flags.NArg() != 0 {
			return errors.New("usage: hmux-agent selector-lines [--mobile] [--selector-sort column] [--selector-direction asc|desc]")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		value, err := agent.CatalogAt(ctx, cfg.StateDir)
		if err != nil {
			return err
		}
		sessions, err := ui.SortSessions(ui.SelectableSessions(value.Sessions), *sortColumn, *sortDirection)
		if err != nil {
			return err
		}
		sessions = ui.FilterSessions(sessions, *selectorQuery)
		fmt.Print(ui.SelectorLinesAtWidth(sessions, *mobile, *selectorWidth))
		return nil
	case "alias-set":
		createdAt, id, err := parseExpectedIdentityArgs(args[1:])
		if err != nil {
			return errors.New("usage: hmux-agent alias-set [--created-at unix-seconds] <stable-session-id>")
		}
		alias, err := readBoundedSingleLine(os.Stdin, 512)
		if err != nil {
			return err
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		if createdAt > 0 {
			return agent.SetAliasExpected(ctx, cfg.StateDir, id, createdAt, alias)
		}
		return agent.SetAlias(ctx, cfg.StateDir, id, alias)
	case "hidden-set":
		createdAt, id, err := parseExpectedIdentityArgs(args[1:])
		if err != nil || createdAt < 1 {
			return errors.New("usage: hmux-agent hidden-set --created-at unix-seconds <stable-session-id>")
		}
		value, err := readBoundedSingleLine(os.Stdin, 16)
		if err != nil {
			return err
		}
		hidden, err := strconv.ParseBool(strings.TrimSpace(value))
		if err != nil {
			return errors.New("hidden state must be true or false")
		}
		cfg, err := config.LoadClient("")
		if err != nil {
			return err
		}
		return agent.SetHiddenExpected(ctx, cfg.StateDir, id, createdAt, hidden)
	case "alias":
		if len(args) == 4 && args[1] == "--inline" {
			cfg, err := config.LoadClient("")
			if err != nil {
				return err
			}
			return agent.SetAlias(ctx, cfg.StateDir, args[2], args[3])
		}
		if len(args) != 2 {
			return errors.New("usage: hmux-agent alias [--inline] <stable-session-id> [alias]")
		}
		return setSessionAlias(ctx, args[1])
	case "terminate":
		if len(args) == 5 && args[1] == "--confirmed" && args[2] == "--created-at" {
			createdAt, err := strconv.ParseInt(args[3], 10, 64)
			if err != nil || createdAt < 1 {
				return errors.New("invalid session creation time")
			}
			return catalog.TerminateSessionExpected(ctx, catalog.TmuxRunner{}, args[4], createdAt)
		}
		if len(args) == 4 && args[1] == "--inline" {
			switch strings.ToLower(strings.TrimSpace(args[3])) {
			case "y", "yes":
				return catalog.TerminateSession(ctx, catalog.TmuxRunner{}, args[2])
			default:
				return errors.New("inline termination requires yes")
			}
		}
		if len(args) == 3 && args[1] == "--confirmed" {
			return catalog.TerminateSession(ctx, catalog.TmuxRunner{}, args[2])
		}
		if len(args) != 2 {
			return errors.New("usage: hmux-agent terminate [--confirmed] <stable-session-id>")
		}
		return terminateSession(ctx, args[1])
	case "selector-footer":
		flags := flag.NewFlagSet("selector-footer", flag.ContinueOnError)
		width := flags.Int("selector-width", 100, "selector footer width")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if flags.NArg() != 0 {
			return errors.New("usage: hmux-agent selector-footer [--selector-width columns]")
		}
		fmt.Print(ui.SelectorFooter("↵ attach   ^N new   ^X terminate   ^R alias   ^Q exit", max(32, *width-8)))
		return nil
	case "version":
		fmt.Printf("hmux-agent %s protocol=%d\n", version, model.ProtocolVersion)
		return nil
	default:
		return usage()
	}
}

func setSessionAlias(ctx context.Context, id string) error {
	session, err := sessionByID(ctx, id)
	if err != nil {
		return err
	}
	alias, err := readTTYLine(fmt.Sprintf(
		"Session: %s (%s)\nCurrent alias: %s\nNew alias (empty = original name): ",
		model.SafeText(session.Name, 512), session.ID, promptValue(session.Alias),
	), 512)
	if err != nil {
		return err
	}
	actionCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	cfg, err := config.LoadClient("")
	if err != nil {
		return err
	}
	if err := agent.SetAliasExpected(actionCtx, cfg.StateDir, session.ID, session.CreatedAt, alias); err != nil {
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

func terminateSession(ctx context.Context, id string) error {
	session, err := sessionByID(ctx, id)
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
	actionCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := catalog.TerminateSessionExpected(actionCtx, catalog.TmuxRunner{}, session.ID, session.CreatedAt); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "Terminated tmux session %q (%s).\n", model.SafeText(name, 512), session.ID)
	return nil
}

func sessionByID(ctx context.Context, id string) (model.Session, error) {
	if err := model.ValidateSessionID(id); err != nil {
		return model.Session{}, err
	}
	cfg, err := config.LoadClient("")
	if err != nil {
		return model.Session{}, err
	}
	value, err := agent.CatalogAt(ctx, cfg.StateDir)
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
	tty, err := os.OpenFile("/dev/tty", os.O_RDWR, 0)
	if err != nil {
		return "", errors.New("interactive terminal is unavailable")
	}
	defer tty.Close()
	if _, err := fmt.Fprint(tty, prompt); err != nil {
		return "", err
	}
	return readSingleLine(tty, maxBytes)
}

func readSingleLine(reader io.Reader, maxBytes int64) (string, error) {
	if maxBytes < 1 {
		return "", errors.New("invalid input limit")
	}
	buffered := bufio.NewReader(io.LimitReader(reader, maxBytes+2))
	line, readErr := buffered.ReadString('\n')
	if readErr != nil && !errors.Is(readErr, io.EOF) {
		return "", readErr
	}
	line = strings.TrimSuffix(strings.TrimSuffix(line, "\n"), "\r")
	if int64(len(line)) > maxBytes {
		return "", fmt.Errorf("input exceeds %d bytes", maxBytes)
	}
	if strings.ContainsAny(line, "\r\n") {
		return "", errors.New("input must be a single line")
	}
	return line, nil
}

func readBoundedSingleLine(reader io.Reader, maxBytes int64) (string, error) {
	if maxBytes < 1 {
		return "", errors.New("invalid input limit")
	}
	data, err := io.ReadAll(io.LimitReader(reader, maxBytes+2))
	if err != nil {
		return "", err
	}
	if int64(len(data)) > maxBytes+1 {
		return "", fmt.Errorf("input exceeds %d bytes", maxBytes)
	}
	text := string(data)
	text = strings.TrimSuffix(text, "\n")
	text = strings.TrimSuffix(text, "\r")
	if int64(len(text)) > maxBytes {
		return "", fmt.Errorf("input exceeds %d bytes", maxBytes)
	}
	if strings.ContainsAny(text, "\r\n") {
		return "", errors.New("input must be a single line")
	}
	return text, nil
}

func promptValue(value string) string {
	value = strings.TrimSpace(model.SafeText(value, 128))
	if value == "" {
		return "(none)"
	}
	return value
}

func parseAttach(args []string) (id, launcherID string, createdAt int64, shared, detach, appView bool, err error) {
	for index := 0; index < len(args); index++ {
		switch args[index] {
		case "--shared":
			shared = true
		case "--detach-others":
			detach = true
		case "--app-view":
			appView = true
		case "--launcher":
			if index+1 >= len(args) {
				return "", "", 0, false, false, false, errors.New("--launcher requires a value")
			}
			launcherID = args[index+1]
			if err := tabstate.ValidateLauncherID(launcherID); err != nil {
				return "", "", 0, false, false, false, err
			}
			index++
		case "--created-at":
			if index+1 >= len(args) || createdAt != 0 {
				return "", "", 0, false, false, false, errors.New("--created-at requires one value")
			}
			createdAt, err = strconv.ParseInt(args[index+1], 10, 64)
			if err != nil || createdAt < 1 {
				return "", "", 0, false, false, false, errors.New("invalid session creation time")
			}
			index++
		default:
			if id != "" {
				return "", "", 0, false, false, false, errors.New("too many attach arguments")
			}
			id = args[index]
		}
	}
	return
}

func parseExpectedIdentityArgs(args []string) (createdAt int64, id string, err error) {
	if len(args) == 1 {
		return 0, args[0], nil
	}
	if len(args) != 3 || args[0] != "--created-at" {
		return 0, "", errors.New("invalid expected identity arguments")
	}
	createdAt, err = strconv.ParseInt(args[1], 10, 64)
	if err != nil || createdAt < 1 {
		return 0, "", errors.New("invalid session creation time")
	}
	return createdAt, args[2], nil
}

func parseCreate(args []string) (profile, name, inventory string, dryRun, nameStdin, jsonOutput bool, err error) {
	inventory = defaultInventoryPath()
	nameSet := false
	for index := 0; index < len(args); index++ {
		switch args[index] {
		case "--name":
			if nameSet || nameStdin {
				return "", "", "", false, false, false, errors.New("--name and --name-stdin are mutually exclusive")
			}
			if index+1 >= len(args) {
				return "", "", "", false, false, false, errors.New("--name requires a value")
			}
			name = args[index+1]
			nameSet = true
			index++
		case "--name-stdin":
			if nameSet || nameStdin {
				return "", "", "", false, false, false, errors.New("--name and --name-stdin are mutually exclusive")
			}
			nameStdin = true
		case "--inventory":
			if index+1 >= len(args) {
				return "", "", "", false, false, false, errors.New("--inventory requires a value")
			}
			inventory = args[index+1]
			index++
		case "--dry-run":
			dryRun = true
		case "--json":
			jsonOutput = true
		default:
			if profile != "" {
				return "", "", "", false, false, false, errors.New("too many create arguments")
			}
			profile = args[index]
		}
	}
	if dryRun && jsonOutput {
		return "", "", "", false, false, false, errors.New("--json cannot be combined with --dry-run")
	}
	return profile, name, inventory, dryRun, nameStdin, jsonOutput, nil
}

func createSessionInline(ctx context.Context, spec string) error {
	spec = strings.TrimSpace(model.SafeText(spec, 512))
	fields := strings.Fields(spec)
	if len(fields) == 0 {
		return errors.New("inline create requires: profile [session name]")
	}
	profileID := fields[0]
	name := strings.TrimSpace(strings.TrimPrefix(spec, profileID))
	inventory, err := config.LoadInventory(defaultInventoryPath())
	if err != nil {
		return err
	}
	cfg, err := config.LoadClient("")
	if err != nil {
		return err
	}
	_, err = agent.Create(ctx, inventory, profileID, name, cfg.StateDir)
	return err
}

func doctor(ctx context.Context) error {
	result := map[string]any{
		"protocol_version": model.ProtocolVersion,
		"version":          version,
		"tmux_env":         os.Getenv("TMUX") != "",
	}
	if path, err := catalog.TmuxPath(); err == nil {
		result["tmux_path"] = path
		output, _ := exec.CommandContext(ctx, path, "-V").Output()
		result["tmux_version"] = strings.TrimSpace(string(output))
	} else {
		result["tmux_error"] = err.Error()
	}
	cfg, cfgErr := config.LoadClient("")
	if cfgErr != nil {
		result["catalog_ok"] = false
		result["catalog_error"] = cfgErr.Error()
	} else if value, err := agent.CatalogAt(ctx, cfg.StateDir); err == nil {
		result["session_count"] = len(value.Sessions)
		result["catalog_ok"] = true
	} else {
		result["catalog_ok"] = false
		result["catalog_error"] = err.Error()
	}
	data, _ := json.MarshalIndent(result, "", "  ")
	fmt.Println(string(data))
	if result["catalog_ok"] != true {
		return errors.New("catalog check failed")
	}
	return nil
}

func defaultInventoryPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".config", "hmux", "inventory.toml")
}

func workflowHook(reader io.Reader, writer io.Writer) {
	defer fmt.Fprintln(writer, "{}")
	event, err := workflow.ParseHook(reader)
	if err != nil {
		return
	}
	cfg, err := config.LoadClient("")
	if err != nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 1500*time.Millisecond)
	defer cancel()
	binding, err := workflow.ResolveBinding(ctx)
	if err != nil {
		return
	}
	_ = (workflow.Store{StateDir: cfg.StateDir}).RecordHook(binding, event)
}

func parseWorkflowArgs(args []string) (filter string, jsonOutput bool, err error) {
	for _, arg := range args {
		if arg == "--json" {
			if jsonOutput {
				return "", false, errors.New("usage: hmux-agent workflow [session] [--json]")
			}
			jsonOutput = true
			continue
		}
		if strings.HasPrefix(arg, "-") || filter != "" {
			return "", false, errors.New("usage: hmux-agent workflow [session] [--json]")
		}
		filter = arg
	}
	return filter, jsonOutput, nil
}

func usage() error {
	return errors.New("usage: hmux-agent <catalog|catalog-stream|recovery|preview|workflow|workflow-hook|workflow-report|attach|select|selector-lines|selector-footer|create|alias|alias-set|hidden-set|terminate|frame-host|frame-ui|frame-inner|frame-spacer|frame-status|frame-click|metadata-migrate|launcher-cleanup|doctor|update|version>")
}

package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/workflow"
)

var version = "dev"

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "hmux-agent:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) == 0 {
		return usage()
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	switch args[0] {
	case "setup-home":
		flags := flag.NewFlagSet("setup-home", flag.ContinueOnError)
		home, err := os.UserHomeDir()
		if err != nil {
			return err
		}
		directory := flags.String("config-dir", filepath.Join(home, ".config", "hmux"), "Home configuration directory")
		workspace := flags.String("workspace-dir", "", "new-session base (new installs: ~/.hmux; existing installs: preserve)")
		if err := flags.Parse(args[1:]); err != nil {
			return err
		}
		if flags.NArg() != 0 {
			return errors.New("unexpected setup-home arguments")
		}
		return config.SetupHome(*directory, *workspace)
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
		cfg, err := config.LoadHome("")
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
		cfg, err := config.LoadHome("")
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
		if len(args) != 1 {
			return errors.New("usage: hmux-agent catalog")
		}
		cfg, err := config.LoadHome("")
		if err != nil {
			return err
		}
		value, err := agent.CatalogAt(ctx, cfg.StateDir)
		if err != nil {
			return err
		}
		data, err := json.MarshalIndent(value, "", "  ")
		if err != nil {
			return err
		}
		fmt.Println(string(data))
		return nil
	case "metadata-migrate":
		clear := len(args) == 2 && args[1] == "--clear"
		if len(args) > 1 && !clear {
			return errors.New("usage: hmux-agent metadata-migrate [--clear]")
		}
		cfg, err := config.LoadHome("")
		if err != nil {
			return err
		}
		count, err := agent.MigrateLegacyMetadata(ctx, cfg.StateDir, clear)
		if err != nil {
			return err
		}
		fmt.Printf("migrated metadata for %d sessions; cleared=%t\n", count, clear)
		return nil
	case "create":
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
		cfg, err := config.LoadHome("")
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
	case "alias-set":
		createdAt, id, err := parseExpectedIdentityArgs(args[1:])
		if err != nil || createdAt < 1 {
			return errors.New("usage: hmux-agent alias-set --created-at unix-seconds <stable-session-id>")
		}
		alias, err := readBoundedSingleLine(os.Stdin, 512)
		if err != nil {
			return err
		}
		cfg, err := config.LoadHome("")
		if err != nil {
			return err
		}
		return agent.SetAliasExpected(ctx, cfg.StateDir, id, createdAt, alias)
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
		cfg, err := config.LoadHome("")
		if err != nil {
			return err
		}
		return agent.SetHiddenExpected(ctx, cfg.StateDir, id, createdAt, hidden)
	case "terminate":
		if len(args) == 5 && args[1] == "--confirmed" && args[2] == "--created-at" {
			createdAt, err := strconv.ParseInt(args[3], 10, 64)
			if err != nil || createdAt < 1 {
				return errors.New("invalid session creation time")
			}
			return catalog.TerminateSessionExpected(ctx, catalog.TmuxRunner{}, args[4], createdAt)
		}
		return errors.New("usage: hmux-agent terminate --confirmed --created-at unix-seconds <session-id>")

	case "version":
		fmt.Printf("hmux-agent %s protocol=%d\n", version, model.ProtocolVersion)
		return nil
	default:
		return usage()
	}
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

func parseExpectedIdentityArgs(args []string) (createdAt int64, id string, err error) {
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
	cfg, cfgErr := config.LoadHome("")
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
	cfg, err := config.LoadHome("")
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
	return errors.New("usage: hmux-agent <catalog|recovery|conversation|workspace|workflow|workflow-hook|workflow-report|setup-home|create|alias-set|hidden-set|terminate|metadata-migrate|doctor|version>")
}

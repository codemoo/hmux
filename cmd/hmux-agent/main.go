package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strconv"
	"time"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
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
		return runSetupHome(ctx, args[1:])
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
		return runWorkflowReport(ctx, args[1:])
	case "workflow":
		return runWorkflow(ctx, args[1:])
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
		return runCreate(ctx, args[1:])
	case "doctor":
		return doctor(ctx)
	case "alias-set":
		return runAliasSet(ctx, args[1:])
	case "hidden-set":
		return runHiddenSet(ctx, args[1:])
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

func usage() error {
	return errors.New("usage: hmux-agent <catalog|recovery|conversation|workspace|workflow|workflow-hook|workflow-report|setup-home|create|alias-set|hidden-set|terminate|metadata-migrate|doctor|version>")
}

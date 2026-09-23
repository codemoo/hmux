package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/workflow"
)

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

func runWorkflowReport(ctx context.Context, args []string) error {
	flags := flag.NewFlagSet("workflow-report", flag.ContinueOnError)
	taskID := flags.String("task-id", "", "sanitized detached task identifier")
	status := flags.String("status", "", "detached task lifecycle status")
	if err := flags.Parse(args); err != nil {
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
}

func runWorkflow(ctx context.Context, args []string) error {
	filter, jsonOutput, err := parseWorkflowArgs(args)
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
}

package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

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

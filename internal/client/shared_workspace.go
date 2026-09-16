package client

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os/exec"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/safeexec"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

func SharedWorkspace(ctx context.Context, cfg config.ClientConfig, change *sharedworkspace.Change) (sharedworkspace.Snapshot, error) {
	if change != nil {
		if err := sharedworkspace.ValidateChange(*change); err != nil {
			return sharedworkspace.Snapshot{}, err
		}
	}
	if cfg.Role == "home" {
		return (sharedworkspace.Store{StateDir: cfg.StateDir}).Sync(ctx, change, func(ctx context.Context) (model.Catalog, error) { return Catalog(ctx, cfg) })
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return sharedworkspace.Snapshot{}, errors.New("unsafe Home configuration")
	}
	if !remoteAgentSupportsCapability(cfg, "shared-workspace-v1") {
		return sharedworkspace.Snapshot{}, errors.New("update Home agent for shared tabs")
	}
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "workspace")
	raw, err := json.Marshal(change)
	if err != nil {
		return sharedworkspace.Snapshot{}, err
	}
	command := exec.CommandContext(ctx, "ssh", args...)
	command.Stdin = bytes.NewReader(raw)
	command.Stderr = io.Discard
	command.WaitDelay = 2 * time.Second
	out, err := safeexec.Output(command, sharedworkspace.MaxBytes)
	if err != nil {
		return sharedworkspace.Snapshot{}, errors.New("shared workspace unavailable")
	}
	var value sharedworkspace.Snapshot
	d := json.NewDecoder(bytes.NewReader(out))
	d.DisallowUnknownFields()
	if err = d.Decode(&value); err != nil {
		return value, err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return value, errors.New("trailing workspace response")
	}
	return value, sharedworkspace.ValidateSnapshot(value)
}

package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/home"
	"github.com/codemoo/hmux/internal/sharedworkspace"
)

func runAgentWorkspace(ctx context.Context, args []string, in io.Reader, out io.Writer) error {
	if len(args) != 0 {
		return errors.New("usage: hmux-agent workspace (JSON on stdin)")
	}
	d := json.NewDecoder(io.LimitReader(in, sharedworkspace.MaxBytes+1))
	d.DisallowUnknownFields()
	var change *sharedworkspace.Change
	if err := d.Decode(&change); err != nil {
		return err
	}
	if d.Decode(&struct{}{}) != io.EOF {
		return errors.New("trailing workspace request")
	}
	cfg, err := config.LoadHome("")
	if err != nil {
		return err
	}
	if cfg.Role != "home" {
		return errors.New("workspace is Home-only")
	}
	value, err := home.SharedWorkspace(ctx, cfg, change)
	if err != nil {
		return err
	}
	return json.NewEncoder(out).Encode(value)
}

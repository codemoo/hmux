package main

import (
	"context"
	"errors"
	"os"
	"strconv"
	"strings"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/config"
)

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

func runAliasSet(ctx context.Context, args []string) error {
	createdAt, id, err := parseExpectedIdentityArgs(args)
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
}

func runHiddenSet(ctx context.Context, args []string) error {
	createdAt, id, err := parseExpectedIdentityArgs(args)
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
}

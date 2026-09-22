package home

import (
	"context"
	"crypto/rand"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
)

func newTerminalViewName() (string, error) {
	var entropy [12]byte
	if _, err := rand.Read(entropy[:]); err != nil {
		return "", fmt.Errorf("generate terminal view identity: %w", err)
	}
	return fmt.Sprintf("hmux-app-view-%d-%x", os.Getpid(), entropy[:]), nil
}

func createExpectedTerminalView(ctx context.Context, runner catalog.Runner, id string, createdAt int64, viewName string) error {
	if err := requireTmuxCreatedAt(ctx, runner, id, createdAt); err != nil {
		return err
	}
	createArgs, err := catalog.TerminalViewCreateArgs(id, viewName)
	if err != nil {
		return err
	}
	if _, err := runner.Output(ctx, createArgs...); err != nil {
		cleanupTerminalView(runner, viewName)
		return fmt.Errorf("create terminal tmux view: %w", err)
	}
	if err := requireTmuxCreatedAt(ctx, runner, id, createdAt); err != nil {
		cleanupTerminalView(runner, viewName)
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

func cleanupTerminalView(runner catalog.Runner, viewName string) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	args, err := catalog.TerminalViewKillArgs(viewName)
	if err == nil {
		_, _ = runner.Output(ctx, args...)
	}
}

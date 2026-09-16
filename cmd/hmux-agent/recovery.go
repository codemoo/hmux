package main

import (
	"context"
	"errors"
	"fmt"
	"io"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/recovery"
)

func runAgentRecovery(ctx context.Context, args []string, out io.Writer) error {
	if len(args) != 1 || (args[0] != "save" && args[0] != "restore" && args[0] != "sync") {
		return errors.New("usage: hmux-agent recovery save|restore|sync")
	}
	cfg, err := config.LoadClient("")
	if err != nil {
		return err
	}
	s := recovery.Store{StateDir: cfg.StateDir, Runner: catalog.TmuxRunner{}, Bind: catalog.ResolveResumeReferences}
	switch args[0] {
	case "save":
		err = s.Save(ctx)
	case "restore":
		err = s.Restore(ctx)
	case "sync":
		err = s.Sync(ctx)
	}
	if err != nil {
		return err
	}
	_, err = fmt.Fprintln(out, "Home recovery checkpoint is up to date.")
	return err
}

package main

import (
	"context"
	"errors"
	"flag"
	"os"
	"path/filepath"

	"github.com/codemoo/hmux/internal/config"
)

func runSetupHome(ctx context.Context, args []string) error {
	flags := flag.NewFlagSet("setup-home", flag.ContinueOnError)
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	directory := flags.String("config-dir", filepath.Join(home, ".config", "hmux"), "Home configuration directory")
	workspace := flags.String("workspace-dir", "", "new-session base (new installs: ~/.hmux; existing installs: preserve)")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if flags.NArg() != 0 {
		return errors.New("unexpected setup-home arguments")
	}
	return config.SetupHome(*directory, *workspace)
}

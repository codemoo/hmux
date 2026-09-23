package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"os"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/homeservice"
	"github.com/codemoo/hmux/internal/webgateway"
)

func connectHome(ctx context.Context, opts webOptions) error {
	var lifecycle func(string)
	if opts.logPath != "" {
		output, err := homeservice.OpenLog(opts.logPath)
		if err != nil {
			return err
		}
		defer output.Close()
		logger := log.New(output, "", log.LstdFlags|log.LUTC)
		lifecycle = func(state string) { logger.Println(state) }
		lifecycle("Starting Home connector")
	}
	token, err := webgateway.LoadToken(opts.tokenPath)
	if err != nil {
		if lifecycle != nil {
			lifecycle("Startup failed: connector token unavailable or invalid")
		}
		return err
	}
	if opts.configPath != "" {
		if _, err := os.Lstat(opts.configPath); err != nil {
			if lifecycle != nil {
				lifecycle("Startup failed: explicit Home configuration missing")
			}
			return err
		}
	}
	cfg, err := config.LoadHome(opts.configPath)
	if err != nil {
		if lifecycle != nil {
			lifecycle("Startup failed: Home configuration unavailable or invalid")
		}
		return err
	}
	fmt.Println("Home connector running; Ctrl-C disconnects web access without ending tmux work.")
	err = webgateway.ConnectHomeLogged(ctx, opts.endpoint, token, cfg, lifecycle)
	if errors.Is(err, context.Canceled) {
		return nil
	}
	if err != nil && lifecycle != nil {
		lifecycle("Home connector failed; check configuration and duplicate processes")
	}
	return err
}

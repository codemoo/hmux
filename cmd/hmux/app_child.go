package main

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// appChildContext binds foreground helpers to the direct app parent as well
// as signals. A normal CLI invocation may omit the parent binding.
func appChildContext(parent context.Context, key string, getenv func(string) string, getParentPID func() int, interval time.Duration) (context.Context, context.CancelFunc, error) {
	signalCtx, stopSignals := signal.NotifyContext(parent, os.Interrupt, syscall.SIGHUP, syscall.SIGTERM)
	ctx, cancel := context.WithCancel(signalCtx)
	stop := func() { cancel(); stopSignals() }
	raw := strings.TrimSpace(getenv(key))
	if raw == "" {
		return ctx, stop, nil
	}
	expected, err := strconv.Atoi(raw)
	if err != nil || expected < 2 || getParentPID == nil || getParentPID() != expected {
		stop()
		return nil, func() {}, fmt.Errorf("%s does not match the direct app parent", key)
	}
	if interval <= 0 {
		interval = 2 * time.Second
	}
	go func() {
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if getParentPID() != expected {
					cancel()
					return
				}
			}
		}
	}()
	return ctx, stop, nil
}

func fileStageAppContext(parent context.Context, getParentPID func() int) (context.Context, context.CancelFunc, error) {
	return appChildContext(parent, "HMUX_FILE_STAGE_PARENT_PID", os.Getenv, getParentPID, fileStageParentPollInterval)
}

func usageAppParentContext(parent context.Context, getParentPID func() int) (context.Context, context.CancelFunc, error) {
	return appChildContext(parent, "HMUX_USAGE_PARENT_PID", os.Getenv, getParentPID, 2*time.Second)
}

func catalogAppParentContext(parent context.Context, getParentPID func() int) (context.Context, context.CancelFunc, error) {
	return appChildContext(parent, "HMUX_CATALOG_PARENT_PID", os.Getenv, getParentPID, 2*time.Second)
}

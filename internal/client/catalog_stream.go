package client

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"time"

	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/hostmetrics"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/recovery"
	"github.com/codemoo/hmux/internal/safeexec"
)

var catalogStreamSourceLease = catalogstream.SourceLease

func CatalogStreamSupported(ctx context.Context, cfg config.ClientConfig) (bool, error) {
	if cfg.Role == "home" {
		return true, nil
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return false, errors.New("unsafe home_alias or agent_path")
	}
	checkCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "capabilities")
	command := exec.CommandContext(checkCtx, "ssh", args...)
	output, err := safeexec.Output(command, 4096)
	if err != nil {
		return false, fmt.Errorf("query remote catalog stream capability: %w", err)
	}
	return capabilityOutputContains(output, "catalog-stream-v1"), nil
}

// StreamCatalogs keeps one SSH process for the lifetime of a remote stream.
// The callback is invoked only for complete, validated, strictly sequenced
// snapshots. Mutations remain on their existing expected-identity commands.
func StreamCatalogs(ctx context.Context, cfg config.ClientConfig, publish func(model.Catalog) error) error {
	return StreamCatalogsObserved(ctx, cfg, nil, publish)
}

// StreamCatalogsObserved runs observe for every complete Home catalog fetch,
// before semantic digest suppression. This lets Home-only event detectors see a
// fast lifecycle transition even when the preceding and following published
// catalogs are semantically identical. Remote JSON catalogs do not retain the
// in-memory pane PIDs required by those detectors.
func StreamCatalogsObserved(ctx context.Context, cfg config.ClientConfig, observe func(context.Context, model.Catalog) error, publish func(model.Catalog) error) error {
	if publish == nil {
		return errors.New("catalog stream publisher is required")
	}
	if cfg.Role == "home" {
		collector := hostmetrics.NewCollector()
		fetch, err := recovery.PrepareCatalog(ctx, cfg.StateDir, func(fetchCtx context.Context) (model.Catalog, error) {
			value, err := Catalog(fetchCtx, cfg)
			if err != nil {
				return value, err
			}
			value.HostMetrics = collector.Sample(fetchCtx)
			return value, nil
		})
		if err != nil {
			return err
		}
		fetch = observeCatalogFetch(fetch, observe)
		return catalogstream.Produce(ctx, catalogstream.DefaultInterval, fetch, func(frame catalogstream.SourceFrame) error {
			if frame.Type == "heartbeat" {
				return nil
			}
			return publish(frame.Catalog)
		})
	}
	if observe != nil {
		return errors.New("catalog stream observer requires Home role")
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	capabilities, err := remoteAgentCapabilities(ctx, cfg)
	if err != nil {
		return fmt.Errorf("query remote catalog stream capabilities: %w", err)
	}
	if !hasCapability(capabilities, "catalog-stream-v1") {
		return errors.New("remote hmux-agent does not support catalog streaming")
	}
	hostMetricsEnabled := hasCapability(capabilities, "host-metrics-v1")
	commandCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	args := append(remoteSSHBaseArgs(cfg.HomeAlias), cfg.AgentPath, "catalog-stream", "--stdio")
	if hostMetricsEnabled {
		args = append(args, "--host-metrics")
	}
	command := exec.CommandContext(commandCtx, "ssh", args...)
	stdin, err := command.StdinPipe()
	if err != nil {
		return err
	}
	stdout, err := command.StdoutPipe()
	if err != nil {
		_ = stdin.Close()
		return err
	}
	// Remote diagnostics are intentionally discarded: they may contain host
	// details and must never block the persistent child on a full pipe.
	command.Stderr = io.Discard
	command.WaitDelay = 2 * time.Second
	if err := command.Start(); err != nil {
		_ = stdin.Close()
		return fmt.Errorf("start remote catalog stream: %w", err)
	}

	type readResult struct {
		frame catalogstream.SourceFrame
		err   error
	}
	results := make(chan readResult, 1)
	readerDone := make(chan struct{})
	go func() {
		defer close(readerDone)
		for {
			frame, readErr := catalogstream.ReadFrame(stdout)
			select {
			case results <- readResult{frame: frame, err: readErr}:
			case <-commandCtx.Done():
				return
			}
			if readErr != nil {
				return
			}
		}
	}()

	lease := catalogStreamSourceLease
	if lease <= 0 {
		lease = catalogstream.SourceLease
	}
	leaseTimer := time.NewTimer(lease)
	defer leaseTimer.Stop()
	resetLease := func() {
		if !leaseTimer.Stop() {
			select {
			case <-leaseTimer.C:
			default:
			}
		}
		leaseTimer.Reset(lease)
	}

	var readErr error
	var expectedSequence uint64 = 1
readLoop:
	for {
		select {
		case <-ctx.Done():
			readErr = ctx.Err()
			break readLoop
		case <-leaseTimer.C:
			readErr = errors.New("remote catalog stream source lease expired")
			break readLoop
		case result := <-results:
			if result.err != nil {
				readErr = result.err
				break readLoop
			}
			resetLease()
			frame := result.frame
			if frame.Sequence != expectedSequence {
				readErr = errors.New("remote catalog stream sequence gap")
				break readLoop
			}
			expectedSequence++
			if frame.Type == "heartbeat" {
				continue
			}
			if !hostMetricsEnabled && frame.Catalog.HostMetrics != nil {
				readErr = errors.New("remote catalog stream returned unsolicited host metrics")
				break readLoop
			}
			value, err := normalizeRemoteCatalog(frame.Catalog, cfg, "")
			if err != nil {
				readErr = err
				break readLoop
			}
			if err := publish(value); err != nil {
				readErr = err
				break readLoop
			}
		}
	}
	cancel()
	_ = stdin.Close()
	waitErr := command.Wait()
	_ = stdout.Close()
	select {
	case <-readerDone:
	case <-time.After(2 * time.Second):
		return errors.New("remote catalog stream reader did not stop")
	}
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if readErr != nil && !errors.Is(readErr, io.EOF) {
		return fmt.Errorf("read remote catalog stream: %w", readErr)
	}
	if waitErr != nil {
		return fmt.Errorf("remote catalog stream exited: %w", waitErr)
	}
	return errors.New("remote catalog stream ended unexpectedly")
}

func observeCatalogFetch(fetch catalogstream.Fetch, observe func(context.Context, model.Catalog) error) catalogstream.Fetch {
	if observe == nil {
		return fetch
	}
	return func(ctx context.Context) (model.Catalog, error) {
		value, err := fetch(ctx)
		if err != nil {
			return value, err
		}
		if err := observe(ctx, value); err != nil {
			return value, err
		}
		return value, nil
	}
}

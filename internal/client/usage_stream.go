package client

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"time"

	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/safeexec"
	usagestream "github.com/codemoo/token-terrier/server-go/stream"
)

var usageStreamSourceLease = usagestream.SourceLease

func UsageStreamSupported(ctx context.Context, cfg config.ClientConfig) (bool, error) {
	return usageStreamCapability(ctx, cfg, "usage-stream-v1")
}

func usageStreamCapability(ctx context.Context, cfg config.ClientConfig, capability string) (bool, error) {
	if cfg.Role == "home" {
		return true, nil
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return false, errors.New("unsafe home_alias or agent_path")
	}
	checkCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	command := exec.CommandContext(
		checkCtx, "ssh",
		"-o", "BatchMode=yes",
		"-o", "ForwardAgent=no",
		"-o", "ClearAllForwardings=yes",
		cfg.HomeAlias, "--", cfg.AgentPath, "capabilities",
	)
	output, err := safeexec.Output(command, 4096)
	if err != nil {
		return false, fmt.Errorf("query remote usage stream capability: %w", err)
	}
	return capabilityOutputContains(output, capability), nil
}

// StreamUsage writes a validated Home-owned usage stream to writer. Local
// callers collect directly as the current user; remote callers reuse the
// already configured HMux SSH identity and never receive provider secrets.
func StreamUsage(ctx context.Context, cfg config.ClientConfig, writer io.Writer) error {
	return streamUsage(ctx, cfg, writer, false)
}

// The web client opts into source-separated quotas; legacy consumers keep
// their original frames. An older remote Home remains a valid legacy source.
func StreamUsageWithSources(ctx context.Context, cfg config.ClientConfig, writer io.Writer) error {
	return streamUsage(ctx, cfg, writer, true)
}

func streamUsage(ctx context.Context, cfg config.ClientConfig, writer io.Writer, sources bool) error {
	if writer == nil {
		return errors.New("usage stream writer is required")
	}
	if cfg.Role == "home" {
		if sources {
			return usagestream.RunWithSources(ctx, writer)
		}
		return usagestream.Run(ctx, writer)
	}
	if !safeAlias(cfg.HomeAlias) || !safeRemotePath(cfg.AgentPath) {
		return errors.New("unsafe home_alias or agent_path")
	}
	commandCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	args := []string{
		"-o", "BatchMode=yes",
		"-o", "ForwardAgent=no",
		"-o", "ClearAllForwardings=yes",
		cfg.HomeAlias, "--", cfg.AgentPath, "usage-stream", "--stdio",
	}
	if sources {
		supported, err := usageStreamCapability(ctx, cfg, "usage-sources-v1")
		if err != nil {
			return err
		}
		if supported {
			args = append(args, "--sources")
		}
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
	// Remote diagnostics can contain local paths and must not block a long-
	// lived child. The protocol itself carries only bounded validated frames.
	command.Stderr = io.Discard
	command.WaitDelay = 2 * time.Second
	if err := command.Start(); err != nil {
		_ = stdin.Close()
		return fmt.Errorf("start remote usage stream: %w", err)
	}

	decoder, err := usagestream.NewDecoder(stdout)
	if err != nil {
		cancel()
		_ = stdin.Close()
		_ = command.Wait()
		return err
	}
	encoder, err := usagestream.NewEncoder(writer)
	if err != nil {
		cancel()
		_ = stdin.Close()
		_ = command.Wait()
		return err
	}
	type readResult struct {
		frame usagestream.Frame
		err   error
	}
	results := make(chan readResult, 1)
	readerDone := make(chan struct{})
	go func() {
		defer close(readerDone)
		for {
			frame, readErr := decoder.Decode()
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

	lease := usageStreamSourceLease
	if lease <= 0 {
		lease = usagestream.SourceLease
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

	var streamErr error
	var expectedSequence uint64 = 1
readLoop:
	for {
		select {
		case <-ctx.Done():
			streamErr = ctx.Err()
			break readLoop
		case <-leaseTimer.C:
			streamErr = errors.New("remote usage stream source lease expired")
			break readLoop
		case result := <-results:
			if result.err != nil {
				streamErr = result.err
				break readLoop
			}
			resetLease()
			if result.frame.Sequence != expectedSequence {
				streamErr = errors.New("remote usage stream sequence gap")
				break readLoop
			}
			expectedSequence++
			if err := encoder.Encode(result.frame); err != nil {
				streamErr = err
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
		return errors.New("remote usage stream reader did not stop")
	}
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if streamErr != nil && !errors.Is(streamErr, io.EOF) {
		return fmt.Errorf("read remote usage stream: %w", streamErr)
	}
	if waitErr != nil {
		return fmt.Errorf("remote usage stream exited: %w", waitErr)
	}
	return errors.New("remote usage stream ended unexpectedly")
}

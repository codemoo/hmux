package main

import (
	"context"
	"errors"
	"io"

	"github.com/codemoo/hmux/internal/config"
	usagestream "github.com/codemoo/token-terrier/server-go/stream"
)

func runAgentUsageStream(ctx context.Context, args []string, stdin io.Reader, stdout io.Writer) error {
	if !validUsageStreamArgs(args) {
		return errors.New("usage: hmux-agent usage-stream --stdio [--sources]")
	}
	cfg, err := config.LoadClient("")
	if err != nil {
		return err
	}
	if cfg.Role != "home" {
		return errors.New("usage stream is available only on the Home Mac")
	}
	return serveAgentUsageStream(ctx, stdin, func(streamCtx context.Context) error {
		if len(args) == 2 {
			return usagestream.RunWithSources(streamCtx, stdout)
		}
		return usagestream.Run(streamCtx, stdout)
	})
}

func validUsageStreamArgs(args []string) bool {
	return (len(args) == 1 || (len(args) == 2 && args[1] == "--sources")) && args[0] == "--stdio"
}

func serveAgentUsageStream(ctx context.Context, stdin io.Reader, produce func(context.Context) error) error {
	if stdin == nil || produce == nil {
		return errors.New("usage stream input and producer are required")
	}
	streamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	inputErrors := make(chan error, 1)
	go func() {
		var one [1]byte
		count, readErr := stdin.Read(one[:])
		var result error
		if count > 0 {
			result = errors.New("usage stream is read-only")
		} else if readErr != nil && !errors.Is(readErr, io.EOF) {
			result = readErr
		}
		inputErrors <- result
		cancel()
	}()
	produceErr := produce(streamCtx)
	select {
	case inputErr := <-inputErrors:
		if inputErr != nil {
			return inputErr
		}
	default:
	}
	if errors.Is(produceErr, context.Canceled) && ctx.Err() == nil {
		return nil
	}
	return produceErr
}

package main

import (
	"context"
	"errors"
	"io"

	"github.com/codemoo/hmux/internal/agent"
	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/hostmetrics"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/recovery"
)

func runAgentCatalogStream(ctx context.Context, args []string, stdin io.Reader, stdout io.Writer) error {
	hostMetricsEnabled, err := parseCatalogStreamArgs(args)
	if err != nil {
		return err
	}
	cfg, err := config.LoadClient("")
	if err != nil {
		return err
	}
	fetch := catalogstream.Fetch(func(fetchCtx context.Context) (model.Catalog, error) {
		return agent.CatalogAt(fetchCtx, cfg.StateDir)
	})
	if hostMetricsEnabled {
		fetch = catalogFetchWithHostMetrics(fetch, hostmetrics.NewCollector())
	}
	return serveAgentCatalogStream(ctx, stdin, func(streamCtx context.Context) error {
		prepared, err := recovery.PrepareCatalog(streamCtx, cfg.StateDir, fetch)
		if err != nil {
			return err
		}
		return catalogstream.Produce(streamCtx, catalogstream.DefaultInterval, prepared, func(frame catalogstream.SourceFrame) error {
			return catalogstream.WriteFrame(stdout, frame)
		})
	})
}

type catalogMetricsSampler interface {
	Sample(context.Context) *model.HostMetrics
}

func parseCatalogStreamArgs(args []string) (bool, error) {
	if len(args) == 1 && args[0] == "--stdio" {
		return false, nil
	}
	if len(args) == 2 && args[0] == "--stdio" && args[1] == "--host-metrics" {
		return true, nil
	}
	return false, errors.New("usage: hmux-agent catalog-stream --stdio [--host-metrics]")
}

func catalogFetchWithHostMetrics(fetch catalogstream.Fetch, sampler catalogMetricsSampler) catalogstream.Fetch {
	return func(ctx context.Context) (model.Catalog, error) {
		value, err := fetch(ctx)
		if err != nil {
			return value, err
		}
		value.HostMetrics = sampler.Sample(ctx)
		return value, nil
	}
}

func serveAgentCatalogStream(ctx context.Context, stdin io.Reader, produce func(context.Context) error) error {
	if stdin == nil || produce == nil {
		return errors.New("catalog stream input and producer are required")
	}
	streamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	inputErrors := make(chan error, 1)
	go func() {
		var one [1]byte
		count, readErr := stdin.Read(one[:])
		var result error
		if count > 0 {
			result = errors.New("catalog stream is read-only")
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

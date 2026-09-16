package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/client"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/model"
)

func runForegroundAppCatalogStream(cfg config.ClientConfig, writer io.Writer) error {
	ctx, stop, err := catalogAppParentContext(context.Background(), os.Getppid)
	if err != nil {
		return err
	}
	defer stop()
	return runAppCatalogStream(ctx, cfg, writer)
}

func runAppCatalogStream(ctx context.Context, cfg config.ClientConfig, writer io.Writer) error {
	supported, err := client.CatalogStreamSupported(ctx, cfg)
	if err != nil {
		return err
	}
	if !supported {
		if err := json.NewEncoder(writer).Encode(catalogstream.UnsupportedBootstrap()); err != nil {
			return err
		}
		closeBootstrapWriter(writer)
		return nil
	}
	broker, err := catalogstream.NewBroker(time.Now())
	if err != nil {
		return err
	}
	defer broker.Close()
	bootstrap := broker.Bootstrap()
	// Source scoping is local app metadata; Home's catalog protocol stays
	// compatible. Failure disables workspace restoration, never guesses a key.
	bootstrap.WorkspaceSourceKey, _ = appWorkspaceSourceKey(ctx, cfg)
	if err := catalogstream.ValidateBootstrap(bootstrap, time.Now()); err != nil {
		return err
	}
	if err := json.NewEncoder(writer).Encode(bootstrap); err != nil {
		return err
	}
	closeBootstrapWriter(writer)

	streamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	updates := make(chan model.Catalog, 1)
	sourceErrors := make(chan error, 1)
	sourceDone := make(chan struct{})
	go func() {
		defer close(sourceDone)
		err := client.StreamCatalogs(streamCtx, cfg, func(value model.Catalog) error {
			if err := validateAppCatalog(value); err != nil {
				return err
			}
			publishLatestCatalog(updates, value)
			return nil
		})
		sourceErrors <- err
	}()
	serveErr := broker.Serve(streamCtx, updates, sourceErrors)
	cancel()
	select {
	case <-sourceDone:
	case <-time.After(3 * time.Second):
		return errors.New("catalog stream source did not stop")
	}
	if errors.Is(serveErr, context.Canceled) && ctx.Err() != nil {
		return ctx.Err()
	}
	return serveErr
}

func publishLatestCatalog(channel chan model.Catalog, value model.Catalog) {
	select {
	case channel <- value:
		return
	default:
	}
	select {
	case <-channel:
	default:
	}
	select {
	case channel <- value:
	default:
	}
}

func closeBootstrapWriter(writer io.Writer) {
	if file, ok := writer.(*os.File); ok && file == os.Stdout {
		if err := file.Close(); err != nil {
			_, _ = fmt.Fprint(io.Discard, err)
		}
	}
}

package home

import (
	"context"
	"errors"

	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/hostmetrics"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/recovery"
)

// StreamCatalogsObserved observes every fetch before unchanged snapshots are suppressed.
func StreamCatalogsObserved(ctx context.Context, cfg config.HomeConfig, observe func(context.Context, model.Catalog) error, publish func(model.Catalog) error) error {
	return StreamCatalogsObservedWithRefresh(ctx, cfg, nil, observe, publish)
}

// StreamCatalogsObservedWithRefresh lets the caller request an immediate
// catalog poll after it changes tmux state.
func StreamCatalogsObservedWithRefresh(ctx context.Context, cfg config.HomeConfig, refresh <-chan struct{}, observe func(context.Context, model.Catalog) error, publish func(model.Catalog) error) error {
	if publish == nil {
		return errors.New("catalog stream publisher is required")
	}
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
	return catalogstream.ProduceWithRefresh(ctx, catalogstream.DefaultInterval, refresh, fetch, func(frame catalogstream.SourceFrame) error {
		if frame.Type == "heartbeat" {
			return nil
		}
		return publish(frame.Catalog)
	})
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

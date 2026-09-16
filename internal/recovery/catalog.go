package recovery

import (
	"context"
	"errors"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/model"
)

// PrepareCatalog is shared by local Home clients and remote agent streams.
// Recovery completes before publication of a catalog that could erase tabs.
func PrepareCatalog(ctx context.Context, stateDir string, fetch catalogstream.Fetch) (catalogstream.Fetch, error) {
	store := Store{StateDir: stateDir, Runner: catalog.TmuxRunner{}, Bind: catalog.ResolveResumeReferences}
	bounded, cancel := context.WithTimeout(ctx, 30*time.Second)
	err := store.Sync(bounded)
	cancel()
	if err != nil {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		return nil, errors.New("Home recovery could not complete; run hmux-agent recovery sync on Home")
	}
	return catalogFetchWithRecovery(fetch, store), nil
}

type recoveryCheckpoint interface{ Save(context.Context) error }

// The Home stream owns checkpoint work; no tab owns a saver or background job.
func catalogFetchWithRecovery(fetch catalogstream.Fetch, store recoveryCheckpoint) catalogstream.Fetch {
	return catalogFetchWithRecoveryClock(fetch, store, time.Now)
}

func catalogFetchWithRecoveryClock(fetch catalogstream.Fetch, store recoveryCheckpoint, now func() time.Time) catalogstream.Fetch {
	last := now()
	return func(ctx context.Context) (model.Catalog, error) {
		if now().Sub(last) >= 30*time.Second {
			checkpointCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
			err := store.Save(checkpointCtx)
			cancel()
			if err == nil {
				last = now()
			}
		}
		return fetch(ctx)
	}
}

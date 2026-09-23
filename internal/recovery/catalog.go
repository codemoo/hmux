package recovery

import (
	"context"
	"errors"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/catalogstream"
)

const checkpointInterval = 30 * time.Second

// PrepareCatalog is shared by local Home clients and remote agent streams.
// Recovery completes before publication of a catalog that could erase tabs.
// The returned close function joins the one periodic checkpoint worker.
func PrepareCatalog(ctx context.Context, stateDir string, fetch catalogstream.Fetch) (catalogstream.Fetch, func(), error) {
	store := Store{StateDir: stateDir, Runner: catalog.TmuxRunner{}, Bind: catalog.ResolveResumeReferences}
	bounded, cancel := context.WithTimeout(ctx, 30*time.Second)
	err := store.Sync(bounded)
	cancel()
	if err != nil {
		if ctx.Err() != nil {
			return nil, nil, ctx.Err()
		}
		return nil, nil, errors.New("Home recovery could not complete; run hmux-agent recovery sync on Home")
	}
	return fetch, startCatalogCheckpoint(ctx, store, checkpointInterval), nil
}

type recoveryCheckpoint interface{ Save(context.Context) error }

// One worker per Home stream saves periodically, independent of publication.
func startCatalogCheckpoint(ctx context.Context, store recoveryCheckpoint, interval time.Duration) func() {
	workerCtx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	go func() {
		defer close(done)
		timer := time.NewTimer(interval)
		defer timer.Stop()
		for {
			select {
			case <-workerCtx.Done():
				return
			case <-timer.C:
			}
			checkpointCtx, stop := context.WithTimeout(workerCtx, 5*time.Second)
			err := store.Save(checkpointCtx)
			stop()
			if workerCtx.Err() != nil {
				return
			}
			if err != nil {
				timer.Reset(5 * time.Second)
			} else {
				timer.Reset(interval)
			}
		}
	}()
	var once sync.Once
	return func() { once.Do(func() { cancel(); <-done }) }
}

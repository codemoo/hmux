package home

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/catalogstream"
	"github.com/codemoo/hmux/internal/model"
)

func TestObservedCatalogFetchRunsBeforeUnchangedSnapshotSuppression(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	value := model.Catalog{
		ProtocolVersion: model.ProtocolVersion,
		Sessions:        []model.Session{{ID: "$1", CreatedAt: 42, PanePID: 123}},
	}
	observed := 0
	fetch := observeCatalogFetch(func(context.Context) (model.Catalog, error) {
		value.GeneratedAt = time.Now().UTC()
		return value, nil
	}, func(_ context.Context, got model.Catalog) error {
		observed++
		if got.Sessions[0].PanePID != 123 {
			t.Fatalf("observer lost in-memory pane PID: %#v", got.Sessions[0])
		}
		if observed == 2 {
			cancel()
		}
		return nil
	})
	published := 0
	err := catalogstream.Produce(ctx, time.Millisecond, fetch, func(frame catalogstream.SourceFrame) error {
		if frame.Type == "snapshot" {
			published++
		}
		return nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("produce error=%v", err)
	}
	if observed != 2 || published != 1 {
		t.Fatalf("observed=%d published=%d", observed, published)
	}
}

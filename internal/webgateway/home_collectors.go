package webgateway

import (
	"context"
	"encoding/json"
	"io"
	"sync"
	"time"

	"github.com/codemoo/hmux/internal/catalog"
	"github.com/codemoo/hmux/internal/config"
	"github.com/codemoo/hmux/internal/home"
	"github.com/codemoo/hmux/internal/hostmetrics"
	"github.com/codemoo/hmux/internal/model"
	"github.com/codemoo/hmux/internal/timing"
	usagestream "github.com/codemoo/token-terrier/server-go/stream"
)

func startHomeCatalogCollectors(ctx context.Context, cancel context.CancelFunc, p *peer, cfg config.HomeConfig, catalogRefresh <-chan struct{}, workers *sync.WaitGroup, failures *homeFailureRecorder) {
	var latestMu sync.Mutex
	var latest json.RawMessage
	tracker := catalog.CompletionTracker{}
	completion := newCompletionWorker(tracker.Observe, p.send)
	workers.Add(1)
	go func() {
		defer workers.Done()
		completion.run(ctx)
	}()
	workers.Add(1)
	go func() {
		defer workers.Done()
		defer cancel()
		err := home.StreamCatalogsObservedWithRefresh(p.timingContext(ctx, "catalog"), cfg, catalogRefresh, completion.enqueue, func(c model.Catalog) error {
			defer timing.Start(p.timingContext(ctx, "catalog"), "catalog-publish", true)()
			// Enrich only the web stream so older strict catalog decoders continue
			// receiving the existing negotiated host-metrics shape.
			if used, total, ok := hostmetrics.DiskUsage(); ok {
				if c.HostMetrics == nil || model.ValidateHostMetrics(c.HostMetrics) != nil {
					c.HostMetrics = &model.HostMetrics{ObservedAt: time.Now().UTC()}
				} else {
					copy := *c.HostMetrics
					c.HostMetrics = &copy
				}
				c.HostMetrics.DiskUsedBytes, c.HostMetrics.DiskTotalBytes = &used, &total
			}
			raw, e := json.Marshal(c)
			if e != nil {
				return e
			}
			latestMu.Lock()
			firstCatalog := latest == nil
			latest = raw
			latestMu.Unlock()
			err := p.send(ctx, Message{Type: "catalog", Payload: raw})
			if err == nil && firstCatalog {
				p.trace("catalog-published", nil)
			}
			if ctx.Err() == nil {
				failures.record("catalog-write", err)
			}
			return err
		})
		if ctx.Err() == nil {
			failures.record("catalog-collector", err)
		}
	}()
	workers.Add(1)
	go func() {
		defer workers.Done()
		tick := time.NewTicker(5 * time.Second)
		defer tick.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-tick.C:
				latestMu.Lock()
				raw := latest
				latestMu.Unlock()
				if raw != nil {
					if err := p.send(ctx, Message{Type: "catalog", Payload: raw}); err != nil {
						if ctx.Err() == nil {
							failures.record("catalog-keepalive-write", err)
						}
						cancel()
						return
					}
				}
			}
		}
	}()
}

func startHomeUsageCollector(ctx context.Context, p *peer, usageRestart <-chan struct{}, workers *sync.WaitGroup) {
	workers.Add(1)
	go func() {
		defer workers.Done()
		for {
			streamCtx, stop := context.WithCancel(ctx)
			reader, writer := io.Pipe()
			var stream sync.WaitGroup
			stream.Add(2)
			go func() {
				defer stream.Done()
				<-streamCtx.Done()
				_ = reader.Close()
				_ = writer.Close()
			}()
			go func() {
				defer stream.Done()
				defer writer.Close()
				_ = usagestream.RunWithSources(streamCtx, writer)
			}()
			ended := make(chan struct{})
			go func() {
				defer close(ended)
				defer reader.Close()
				decoder, err := usagestream.NewDecoder(reader)
				if err != nil {
					return
				}
				for {
					f, err := decoder.Decode()
					if err != nil {
						return
					}
					if f.Type == "snapshot" && p.send(ctx, Message{Type: "usage", Payload: f.Snapshot}) != nil {
						return
					}
				}
			}()
			restart := false
			select {
			case <-ctx.Done():
			case <-usageRestart:
				restart = true
			case <-ended:
			}
			stop()
			<-ended
			stream.Wait()
			if !restart {
				if ctx.Err() == nil {
					_ = p.send(ctx, Message{Type: "usage-unavailable"})
				}
				return
			}
		}
	}()
}

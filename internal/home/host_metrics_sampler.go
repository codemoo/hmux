package home

import (
	"context"
	"sync"
	"sync/atomic"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

// One sampler belongs to a Home stream. Catalog reads never wait for the
// platform's CPU/GPU commands, and each observation keeps its sample time.
type hostMetricsSampler struct {
	latest atomic.Pointer[model.HostMetrics]
	cancel context.CancelFunc
	done   chan struct{}
	once   sync.Once
}

func startHostMetricsSampler(ctx context.Context, interval time.Duration, sample func(context.Context) *model.HostMetrics) *hostMetricsSampler {
	workerCtx, cancel := context.WithCancel(ctx)
	s := &hostMetricsSampler{cancel: cancel, done: make(chan struct{})}
	go func() {
		defer close(s.done)
		timer := time.NewTimer(0)
		defer timer.Stop()
		for {
			select {
			case <-workerCtx.Done():
				return
			case <-timer.C:
			}
			s.latest.Store(sample(workerCtx))
			if workerCtx.Err() != nil {
				return
			}
			timer.Reset(interval)
		}
	}()
	return s
}

func (s *hostMetricsSampler) Latest() *model.HostMetrics { return s.latest.Load() }

func (s *hostMetricsSampler) Close() { s.once.Do(func() { s.cancel(); <-s.done }) }

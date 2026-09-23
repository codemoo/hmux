package home

import (
	"context"
	"testing"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

func TestHostMetricsSamplerPublishesLastTimestampWithoutBlocking(t *testing.T) {
	started := make(chan struct{})
	observed := time.Date(2026, 9, 23, 1, 2, 3, 0, time.UTC)
	sampler := startHostMetricsSampler(t.Context(), time.Hour, func(ctx context.Context) *model.HostMetrics {
		close(started)
		<-ctx.Done()
		cpu := 42.0
		return &model.HostMetrics{ObservedAt: observed, CPUPercent: &cpu}
	})
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("sampler did not start")
	}
	if sampler.Latest() != nil {
		t.Fatal("unfinished sample was published")
	}
	closed := make(chan struct{})
	go func() { sampler.Close(); close(closed) }()
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("sampler did not join")
	}
	if got := sampler.Latest(); got == nil || !got.ObservedAt.Equal(observed) || got.CPUPercent == nil || *got.CPUPercent != 42 {
		t.Fatalf("latest sample = %#v", got)
	}
}

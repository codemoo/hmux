// Package timing carries optional, content-free Home latency observations.
package timing

import (
	"context"
	"time"
)

type key struct{}
type Reporter func(stage string, duration time.Duration)

func WithReporter(ctx context.Context, report Reporter) context.Context {
	return context.WithValue(ctx, key{}, report)
}

// Start accepts only static internal stage names. Fast nested spans are omitted
// to bound log volume; callers may require top-level lifecycle spans.
func Start(ctx context.Context, stage string, always bool) func() {
	report, _ := ctx.Value(key{}).(Reporter)
	if report == nil {
		return func() {}
	}
	start := time.Now()
	return func() {
		elapsed := time.Since(start)
		if always || elapsed >= 250*time.Millisecond {
			report(stage, elapsed)
		}
	}
}

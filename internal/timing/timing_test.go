package timing

import (
	"context"
	"testing"
	"time"
)

func TestOptionalTimingAndThreshold(t *testing.T) {
	Start(context.Background(), "unused", true)()
	var stages []string
	ctx := WithReporter(context.Background(), func(stage string, duration time.Duration) {
		if duration < 0 {
			t.Fatal("negative duration")
		}
		stages = append(stages, stage)
	})
	Start(ctx, "fast", false)()
	Start(ctx, "mandatory", true)()
	if len(stages) != 1 || stages[0] != "mandatory" {
		t.Fatal(stages)
	}
	inherited, cancel := context.WithCancel(ctx)
	cancel()
	Start(inherited, "cancelled", true)()
	if len(stages) != 2 {
		t.Fatal(stages)
	}
}

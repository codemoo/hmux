//go:build linux

package hostmetrics

import (
	"context"
	"os"
	"time"
)

// cpuInterval is the gap between the two /proc/stat readings of one sample.
const cpuInterval = 250 * time.Millisecond

func newPlatformSampler() sampler {
	return func(ctx context.Context) measurements {
		var value measurements
		if first, err := os.ReadFile("/proc/stat"); err == nil {
			select {
			case <-ctx.Done():
			case <-time.After(cpuInterval):
				if second, err := os.ReadFile("/proc/stat"); err == nil {
					value.cpuPercent = cpuPercentBetween(first, second)
				}
			}
		}
		if raw, err := os.ReadFile("/proc/meminfo"); err == nil {
			value.memoryUsedBytes, value.memoryTotalBytes = parseMeminfo(raw)
		}
		return value
	}
}

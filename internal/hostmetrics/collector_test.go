package hostmetrics

import (
	"context"
	"math"
	"testing"
	"time"
)

func pointer[T any](value T) *T { return &value }

func TestCollectorOmitsInvalidFieldsWithoutKeepingStaleValues(t *testing.T) {
	now := time.Date(2026, 9, 8, 12, 0, 0, 123, time.UTC)
	count := 0
	collector := &Collector{
		now: func() time.Time { return now },
		sample: func(context.Context) measurements {
			count++
			if count == 1 {
				return measurements{
					cpuPercent: pointer(25.5), gpuPercent: pointer(50.0),
					memoryUsedBytes: pointer(uint64(4)), memoryTotalBytes: pointer(uint64(8)),
				}
			}
			return measurements{
				cpuPercent: pointer(math.NaN()), gpuPercent: pointer(101.0),
				memoryUsedBytes: pointer(uint64(9)), memoryTotalBytes: pointer(uint64(8)),
			}
		},
	}
	first := collector.Sample(context.Background())
	if first == nil || first.CPUPercent == nil || first.GPUPercent == nil || first.MemoryUsedBytes == nil || !first.ObservedAt.Equal(now) {
		t.Fatalf("first sample=%#v", first)
	}
	if second := collector.Sample(context.Background()); second != nil {
		t.Fatalf("invalid sample retained data: %#v", second)
	}
}

func TestBuildHostMetricsKeepsValidSamplersWhenAnotherFails(t *testing.T) {
	value := buildHostMetrics(time.Now(), measurements{
		cpuPercent: pointer(37.0),
		gpuPercent: pointer(math.Inf(1)),
	})
	if value == nil || value.CPUPercent == nil || *value.CPUPercent != 37 || value.GPUPercent != nil {
		t.Fatalf("partial sample=%#v", value)
	}
}

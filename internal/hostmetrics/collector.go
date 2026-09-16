package hostmetrics

import (
	"context"
	"math"
	"time"

	"github.com/codemoo/hmux/internal/model"
)

const sampleTimeout = 3 * time.Second

type measurements struct {
	cpuPercent       *float64
	gpuPercent       *float64
	memoryUsedBytes  *uint64
	memoryTotalBytes *uint64
}

type sampler func(context.Context) measurements

// Collector owns one Home-host sampler for the lifetime of a catalog stream.
// It keeps no last-known values: an unavailable field is omitted from that
// observation instead of leaking a stale or wrong-source measurement.
type Collector struct {
	sample sampler
	now    func() time.Time
}

func NewCollector() *Collector {
	return &Collector{sample: newPlatformSampler(), now: time.Now}
}

// Sample is fail-open for the catalog stream. Individual invalid or failed
// samplers are absent, and a fully unavailable observation is nil.
func (c *Collector) Sample(ctx context.Context) *model.HostMetrics {
	if c == nil || c.sample == nil {
		return nil
	}
	sampleCtx, cancel := context.WithTimeout(ctx, sampleTimeout)
	defer cancel()
	value := c.sample(sampleCtx)
	return buildHostMetrics(c.now().UTC(), value)
}

func buildHostMetrics(observedAt time.Time, value measurements) *model.HostMetrics {
	result := &model.HostMetrics{ObservedAt: observedAt.UTC()}
	if validPercent(value.cpuPercent) {
		result.CPUPercent = value.cpuPercent
	}
	if validPercent(value.gpuPercent) {
		result.GPUPercent = value.gpuPercent
	}
	if value.memoryUsedBytes != nil && value.memoryTotalBytes != nil &&
		*value.memoryTotalBytes > 0 && *value.memoryTotalBytes <= model.MaximumHostMemoryBytes &&
		*value.memoryUsedBytes <= *value.memoryTotalBytes {
		result.MemoryUsedBytes = value.memoryUsedBytes
		result.MemoryTotalBytes = value.memoryTotalBytes
	}
	if result.CPUPercent == nil && result.GPUPercent == nil && result.MemoryUsedBytes == nil {
		return nil
	}
	if err := model.ValidateHostMetrics(result); err != nil {
		return nil
	}
	return result
}

func validPercent(value *float64) bool {
	return value != nil && !math.IsNaN(*value) && !math.IsInf(*value, 0) && *value >= 0 && *value <= 100
}

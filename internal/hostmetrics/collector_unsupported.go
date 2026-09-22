//go:build !darwin && !linux

package hostmetrics

func newPlatformSampler() sampler {
	return nil
}

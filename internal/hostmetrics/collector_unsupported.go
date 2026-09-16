//go:build !darwin

package hostmetrics

func newPlatformSampler() sampler {
	return nil
}

//go:build !darwin

package hostmetrics

func DiskUsage() (used, total uint64, ok bool) { return 0, 0, false }

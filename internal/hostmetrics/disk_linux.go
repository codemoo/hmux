//go:build linux

package hostmetrics

import "golang.org/x/sys/unix"

// DiskUsage samples the filesystem holding the root directory.
func DiskUsage() (used, total uint64, ok bool) {
	var value unix.Statfs_t
	if err := unix.Statfs("/", &value); err != nil {
		return 0, 0, false
	}
	return diskBytes(value.Blocks, value.Bfree, uint64(value.Bsize))
}
